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
