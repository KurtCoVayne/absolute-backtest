//! The lane-interleaved block (ST-4.2): 1,024 offsets as 32 lanes of 32
//! values, value `j` in lane `j mod 32` at row `j div 32`, row `i` of a lane
//! at bits `[i·W, (i+1)·W)` of that lane's stream of `W`-bit values, and
//! the lanes' words interleaved (word `r` of lane `l` is `words[r·32 + l]`),
//! so that unpacking a row is the same shift and mask on 32 adjacent words.
//! Words are 32 bits to width 32 and 64 bits above (a lane then holds
//! `⌈W/2⌉` words, half a word of padding for an odd width). The fully
//! unrolled kernel (`unpack`, one per width, shifts as constants) decodes a
//! whole block; the row-range kernel (`unpack_rows`) decodes the rows a
//! short range needs; `extract` reads one value from one or two words.
//! Patched exceptions (ST-4.4) are written back by `patch`. The tail
//! (ST-8.3) is packed horizontally, value `j` at bits `[j·W, (j+1)·W)` of
//! one stream, so an append writes in place.

use crate::tick::Exception;

/// The lanes of a block and the values each holds.
pub const LANES: usize = 32;
pub const ROWS: usize = 32;

/// A block's packed words.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Words {
    /// Widths 0 to 32: `32·W` words.
    W32(Vec<u32>),
    /// Widths 33 to 64: `32·⌈W/2⌉` words.
    W64(Vec<u64>),
}

impl Words {
    pub fn byte_len(&self) -> usize {
        match self {
            Words::W32(w) => w.len() * 4,
            Words::W64(w) => w.len() * 8,
        }
    }
}

/// A packed block: its width and words.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Packed {
    pub width: u8,
    pub words: Words,
}

impl Packed {
    /// The words as little-endian bytes (the segment's form).
    pub fn to_bytes(&self) -> Vec<u8> {
        match &self.words {
            Words::W32(w) => w.iter().flat_map(|x| x.to_le_bytes()).collect(),
            Words::W64(w) => w.iter().flat_map(|x| x.to_le_bytes()).collect(),
        }
    }

    /// A block's words from their little-endian bytes at `width`.
    pub fn from_bytes(width: u8, bytes: &[u8]) -> Packed {
        assert!(width <= 64);
        let words = if width == 0 {
            Words::W32(Vec::new())
        } else if width <= 32 {
            let n = LANES * width as usize;
            assert!(bytes.len() >= n * 4, "{} bytes for width {}", bytes.len(), width);
            Words::W32((0..n).map(|i| u32::from_le_bytes(bytes[i * 4..i * 4 + 4].try_into().unwrap())).collect())
        } else {
            let n = LANES * (width as usize).div_ceil(2);
            assert!(bytes.len() >= n * 8, "{} bytes for width {}", bytes.len(), width);
            Words::W64((0..n).map(|i| u64::from_le_bytes(bytes[i * 8..i * 8 + 8].try_into().unwrap())).collect())
        };
        Packed { width, words }
    }
}

fn mask(width: u8) -> u64 {
    if width >= 64 {
        u64::MAX
    } else {
        (1u64 << width) - 1
    }
}

/// Packs up to 1,024 offsets, each below `2^width`, into the lane layout.
pub fn pack(offsets: &[u64], width: u8) -> Packed {
    assert!(offsets.len() <= crate::BLOCK);
    assert!(width <= 64);
    let w = width as usize;
    if width == 0 {
        return Packed { width, words: Words::W32(Vec::new()) };
    }
    if width <= 32 {
        let mut words = vec![0u32; LANES * w];
        for (j, &u) in offsets.iter().enumerate() {
            debug_assert!(u <= mask(width), "offset {} is above width {}", u, width);
            let (lane, row) = (j % LANES, j / LANES);
            let b = row * w;
            let (r0, s0) = (b / 32, b % 32);
            words[r0 * LANES + lane] |= (u << s0) as u32;
            if s0 + w > 32 {
                words[(r0 + 1) * LANES + lane] |= (u >> (32 - s0)) as u32;
            }
        }
        Packed { width, words: Words::W32(words) }
    } else {
        let per_lane = w.div_ceil(2);
        let mut words = vec![0u64; LANES * per_lane];
        for (j, &u) in offsets.iter().enumerate() {
            let (lane, row) = (j % LANES, j / LANES);
            let b = row * w;
            let (r0, s0) = (b / 64, b % 64);
            words[r0 * LANES + lane] |= u << s0;
            if s0 + w > 64 {
                words[(r0 + 1) * LANES + lane] |= u >> (64 - s0);
            }
        }
        Packed { width, words: Words::W64(words) }
    }
}

/// Row `i` of every lane into `out[i·32 ..]`, for 32-bit words: the shift
/// is one value for the 32 lanes, so the lane loop vectorises.
#[inline(always)]
fn row32<const W: usize>(words: &[u32], i: usize, out: &mut [u64]) {
    let b = i * W;
    let (r0, s0) = (b / 32, b % 32);
    let m = mask(W as u8);
    let lo = &words[r0 * LANES..r0 * LANES + LANES];
    if s0 + W <= 32 {
        for l in 0..LANES {
            out[i * LANES + l] = ((lo[l] >> s0) as u64) & m;
        }
    } else {
        let hi = &words[(r0 + 1) * LANES..(r0 + 2) * LANES];
        for l in 0..LANES {
            out[i * LANES + l] = (((lo[l] >> s0) as u64) | ((hi[l] as u64) << (32 - s0))) & m;
        }
    }
}

#[inline(always)]
fn row64<const W: usize>(words: &[u64], i: usize, out: &mut [u64]) {
    let b = i * W;
    let (r0, s0) = (b / 64, b % 64);
    let m = mask(W as u8);
    let lo = &words[r0 * LANES..r0 * LANES + LANES];
    if s0 + W <= 64 {
        for l in 0..LANES {
            out[i * LANES + l] = (lo[l] >> s0) & m;
        }
    } else {
        let hi = &words[(r0 + 1) * LANES..(r0 + 2) * LANES];
        for l in 0..LANES {
            out[i * LANES + l] = ((lo[l] >> s0) | (hi[l] << (64 - s0))) & m;
        }
    }
}

/// The whole block at a compile-time width: every row's shifts are
/// constants after unrolling.
fn unpack_const<const W: usize>(p: &Packed, out: &mut [u64]) {
    match &p.words {
        Words::W32(words) => {
            for i in 0..ROWS {
                row32::<W>(words, i, out);
            }
        }
        Words::W64(words) => {
            for i in 0..ROWS {
                row64::<W>(words, i, out);
            }
        }
    }
}

macro_rules! dispatch {
    ($p:expr, $out:expr, $($w:literal),*) => {
        match $p.width {
            $($w => unpack_const::<$w>($p, $out),)*
            _ => unreachable!("a width above 64"),
        }
    };
}

/// Decodes the whole block (1,024 offsets) into `out`, with the kernel of
/// its width (ST-4.2: the fully unrolled form).
pub fn unpack(p: &Packed, out: &mut [u64; crate::BLOCK]) {
    if p.width == 0 {
        out.fill(0);
        return;
    }
    dispatch!(
        p, out, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30, 31, 32, 33, 34, 35, 36, 37, 38, 39, 40, 41, 42, 43, 44, 45, 46, 47, 48,
        49, 50, 51, 52, 53, 54, 55, 56, 57, 58, 59, 60, 61, 62, 63, 64
    )
}

/// The rows a value range `[a, b)` of the block spans: lane rows
/// `a div 32` to `⌈b / 32⌉`.
pub fn rows_of(a: usize, b: usize) -> (usize, usize) {
    (a / LANES, b.div_ceil(LANES))
}

/// Decodes the values `[a, b)` of the block into `out[.. b − a]` with the
/// row-range kernel (shifts computed at run time): the lane rows the range
/// spans, the lanes outside it masked by the copy.
pub fn unpack_rows(p: &Packed, a: usize, b: usize, out: &mut [u64]) {
    assert!(a <= b && b <= crate::BLOCK);
    let w = p.width as usize;
    let (ra, rb) = rows_of(a, b);
    let mut rows = [0u64; crate::BLOCK];
    if w > 0 {
        for i in ra..rb {
            let b0 = i * w;
            match &p.words {
                Words::W32(words) => {
                    let (r0, s0) = (b0 / 32, b0 % 32);
                    let m = mask(p.width);
                    let lo = &words[r0 * LANES..r0 * LANES + LANES];
                    if s0 + w <= 32 {
                        for l in 0..LANES {
                            rows[i * LANES + l] = ((lo[l] >> s0) as u64) & m;
                        }
                    } else {
                        let hi = &words[(r0 + 1) * LANES..(r0 + 2) * LANES];
                        for l in 0..LANES {
                            rows[i * LANES + l] = (((lo[l] >> s0) as u64) | ((hi[l] as u64) << (32 - s0))) & m;
                        }
                    }
                }
                Words::W64(words) => {
                    let (r0, s0) = (b0 / 64, b0 % 64);
                    let m = mask(p.width);
                    let lo = &words[r0 * LANES..r0 * LANES + LANES];
                    if s0 + w <= 64 {
                        for l in 0..LANES {
                            rows[i * LANES + l] = (lo[l] >> s0) & m;
                        }
                    } else {
                        let hi = &words[(r0 + 1) * LANES..(r0 + 2) * LANES];
                        for l in 0..LANES {
                            rows[i * LANES + l] = ((lo[l] >> s0) | (hi[l] << (64 - s0))) & m;
                        }
                    }
                }
            }
        }
    }
    out[..b - a].copy_from_slice(&rows[a..b]);
}

/// One value, from one or two words.
pub fn extract(p: &Packed, j: usize) -> u64 {
    let w = p.width as usize;
    if w == 0 {
        return 0;
    }
    let (lane, row) = (j % LANES, j / LANES);
    let b = row * w;
    match &p.words {
        Words::W32(words) => {
            let (r0, s0) = (b / 32, b % 32);
            let lo = (words[r0 * LANES + lane] >> s0) as u64;
            let v = if s0 + w > 32 { lo | ((words[(r0 + 1) * LANES + lane] as u64) << (32 - s0)) } else { lo };
            v & mask(p.width)
        }
        Words::W64(words) => {
            let (r0, s0) = (b / 64, b % 64);
            let lo = words[r0 * LANES + lane] >> s0;
            let v = if s0 + w > 64 { lo | (words[(r0 + 1) * LANES + lane] << (64 - s0)) } else { lo };
            v & mask(p.width)
        }
    }
}

/// Writes the patched offsets whose positions fall in `[a, b)` into the
/// decoded values `out` of that range (`out[0]` is row `a`); exceptions are
/// sorted by position, so the range's are found by binary search (ST-4.4).
/// Raw exceptions (values off the grid) are the reader's to return by their
/// bits; here they are skipped.
pub fn patch(exceptions: &[Exception], a: usize, b: usize, out: &mut [u64]) {
    let start = exceptions.partition_point(|x| (x.position() as usize) < a);
    for x in &exceptions[start..] {
        let pos = x.position() as usize;
        if pos >= b {
            break;
        }
        if let Exception::Offset { offset, .. } = x {
            out[pos - a] = *offset;
        }
    }
}

/// The integer view of rows `[a, b)` with every raw exception (a value off
/// any grid, kept as its bits) set to 0: the number the block's summaries
/// and cumulatives count for it (A-D54), so that a decoded block and the
/// structures over it agree. `ints` holds the rows' integers as the
/// header's formula gives them (the packed 0 of such a row is the base).
pub fn zero_raw(exceptions: &[Exception], a: usize, b: usize, ints: &mut [i64]) {
    let start = exceptions.partition_point(|x| (x.position() as usize) < a);
    for x in &exceptions[start..] {
        let pos = x.position() as usize;
        if pos >= b {
            break;
        }
        if let Exception::Raw { .. } = x {
            ints[pos - a] = 0;
        }
    }
}

/// The tail block (ST-8.3): up to 1,023 values packed horizontally, value
/// `j` at bits `[j·W, (j+1)·W)` of one stream, so that a value on the grid
/// and below the width is appended in place.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Horizontal {
    pub width: u8,
    pub n: usize,
    pub words: Vec<u64>,
}

impl Horizontal {
    pub fn new(width: u8) -> Horizontal {
        Horizontal {
            width,
            n: 0,
            words: vec![0u64; (crate::BLOCK * width as usize).div_ceil(64)],
        }
    }

    /// Packs values at a width (they must fit).
    pub fn of(values: &[u64], width: u8) -> Horizontal {
        let mut h = Horizontal::new(width);
        for &v in values {
            h.push(v);
        }
        h
    }

    pub fn get(&self, j: usize) -> u64 {
        assert!(j < self.n);
        let w = self.width as usize;
        if w == 0 {
            return 0;
        }
        let b = j * w;
        let (r0, s0) = (b / 64, b % 64);
        let lo = self.words[r0] >> s0;
        let v = if s0 + w > 64 { lo | (self.words[r0 + 1] << (64 - s0)) } else { lo };
        v & mask(self.width)
    }

    /// Whether `u` can be appended in place (it fits the width and the block).
    pub fn fits(&self, u: u64) -> bool {
        self.n < crate::BLOCK && u <= mask(self.width)
    }

    /// Appends `u` in place; the caller checked `fits`.
    pub fn push(&mut self, u: u64) {
        assert!(self.fits(u), "the tail cannot take {} at width {} with {} values", u, self.width, self.n);
        let w = self.width as usize;
        if w > 0 {
            let b = self.n * w;
            let (r0, s0) = (b / 64, b % 64);
            self.words[r0] |= u << s0;
            if s0 + w > 64 {
                self.words[r0 + 1] |= u >> (64 - s0);
            }
        }
        self.n += 1;
    }

    /// The first `n` values (a reader's visible prefix).
    pub fn values(&self, n: usize) -> Vec<u64> {
        (0..n.min(self.n)).map(|j| self.get(j)).collect()
    }
}

/// A block's header as a segment stores it (ST-4.3): 32 bytes, little
/// endian, kept apart from the packed words in a header array per column.
/// `flags` carry `recip_exact`, `patched` (the block has exceptions),
/// `predicted` (a residual column of ST-4.5) and `f32` (the source wrote
/// `f32` values, so the view is the decoded value rounded to `f32`);
/// `exceptions` is the offset of the block's exception list in its
/// segment's exception area, 0 when it has none.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Header {
    pub base: i64,
    pub g: u64,
    pub e: i8,
    pub width: u8,
    pub n: u16,
    pub flags: u16,
    pub exceptions: u32,
}

pub const HEADER_BYTES: usize = 32;
pub const FLAG_RECIP_EXACT: u16 = 1;
pub const FLAG_PATCHED: u16 = 2;
pub const FLAG_PREDICTED: u16 = 4;
pub const FLAG_F32: u16 = 8;
/// The block holds raw-bit exceptions: values off any grid, so no exact
/// integer view of the block exists.
pub const FLAG_RAW: u16 = 16;

impl Header {
    pub fn to_bytes(&self) -> [u8; HEADER_BYTES] {
        let mut b = [0u8; HEADER_BYTES];
        b[0..8].copy_from_slice(&self.base.to_le_bytes());
        b[8..16].copy_from_slice(&self.g.to_le_bytes());
        b[16] = self.e as u8;
        b[17] = self.width;
        b[18..20].copy_from_slice(&self.n.to_le_bytes());
        b[20..22].copy_from_slice(&self.flags.to_le_bytes());
        b[22..26].copy_from_slice(&self.exceptions.to_le_bytes());
        b
    }

    pub fn from_bytes(b: &[u8]) -> Header {
        assert!(b.len() >= HEADER_BYTES);
        Header {
            base: i64::from_le_bytes(b[0..8].try_into().unwrap()),
            g: u64::from_le_bytes(b[8..16].try_into().unwrap()),
            e: b[16] as i8,
            width: b[17],
            n: u16::from_le_bytes(b[18..20].try_into().unwrap()),
            flags: u16::from_le_bytes(b[20..22].try_into().unwrap()),
            exceptions: u32::from_le_bytes(b[22..26].try_into().unwrap()),
        }
    }

    pub fn recip_exact(&self) -> bool {
        self.flags & FLAG_RECIP_EXACT != 0
    }

    pub fn patched(&self) -> bool {
        self.flags & FLAG_PATCHED != 0
    }

    pub fn predicted(&self) -> bool {
        self.flags & FLAG_PREDICTED != 0
    }

    pub fn f32(&self) -> bool {
        self.flags & FLAG_F32 != 0
    }

    pub fn raw(&self) -> bool {
        self.flags & FLAG_RAW != 0
    }

    /// The header of an encoded block, its exception offset to be set by
    /// the segment writer.
    pub fn of(enc: &crate::tick::Encoded, predicted: bool) -> Header {
        let mut flags = 0u16;
        if enc.recip_exact {
            flags |= FLAG_RECIP_EXACT;
        }
        if !enc.exceptions.is_empty() {
            flags |= FLAG_PATCHED;
        }
        if predicted {
            flags |= FLAG_PREDICTED;
        }
        if enc.f32 {
            flags |= FLAG_F32;
        }
        if !enc.exact() {
            flags |= FLAG_RAW;
        }
        Header {
            base: enc.base,
            g: enc.g,
            e: enc.e as i8,
            width: enc.width,
            n: enc.n,
            flags,
            exceptions: 0,
        }
    }

    /// The packed words a block of this header takes, in bytes.
    pub fn words_bytes(&self) -> usize {
        let w = self.width as usize;
        if w == 0 {
            0
        } else if w <= 32 {
            LANES * w * 4
        } else {
            LANES * w.div_ceil(2) * 8
        }
    }
}
