//! Segment files (ST-4.8, ST-7.1; A-D48): the immutable, content-addressed
//! files that hold the sealed blocks of one (relation, level, column) for
//! an instrument range, mapped read-only and shared between versions by
//! hash. Little-endian throughout.
//!
//! ```text
//! 0      magic "ABTSEG\0\x01", codec version u16, flags u16, reserved u32   (16 bytes)
//! 16     the runs, each an instrument's consecutive blocks:
//!          headers      blocks × 32 bytes (ST-4.3)
//!          words        each block's packed words, 64-byte aligned
//!          exceptions   per block with any: count u32, then 16 bytes each
//!                       (position u16, kind u8, pad, value u64); a block's
//!                       header holds 1 + its offset here, 0 for none
//!          summaries    blocks × 48 bytes (n, argmin, argmax, min, max, first, last)
//!          cumulatives  (blocks + 1) × 88 bytes (count, Σx i128, Σx² and Σk·x
//!                       as 256-bit two's complement), the totals before each
//!                       block and after the last, from the instrument's first row
//! footer the run table: 64 bytes per run (instrument, first block, blocks,
//!        exponent, the five offsets, rows)
//! trailer runs u64, footer offset u64, BLAKE3 of everything before it (32
//!        bytes), magic "ABTSEGFT"
//! ```
//!
//! A block's hash, the unit of a version's identity (Proposition 7.1), is
//! BLAKE3 over its header, words and exception bytes.

use abt_agg::exact::{Int, Wide};
use abt_codec::block::{pack, unpack, Header, Packed, HEADER_BYTES};
use abt_codec::summary::{BlockSummary, Cumulative};
use abt_codec::tick::{Encoded, Exception};
use abt_codec::BLOCK;

use crate::error::DataError;

pub const MAGIC: &[u8; 8] = b"ABTSEG\0\x01";
pub const TRAILER_MAGIC: &[u8; 8] = b"ABTSEGFT";
pub const PREAMBLE: usize = 16;
pub const RUN_ENTRY: usize = 64;
pub const TRAILER: usize = 8 + 8 + 32 + 8;
pub const SUMMARY_BYTES: usize = 48;
pub const CUMULATIVE_BYTES: usize = 88;
pub const EXCEPTION_BYTES: usize = 16;
const ALIGN: usize = 64;

/// A run: one instrument's consecutive blocks in a segment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Run {
    pub instrument: u32,
    pub first_block: u32,
    pub blocks: u32,
    /// The exponent the run's summaries and cumulatives are at.
    pub exponent: u8,
    pub headers: u64,
    pub words: u64,
    pub exceptions: u64,
    pub summaries: u64,
    pub cumulatives: u64,
    pub rows: u64,
}

/// A block as the writer takes it: its encoding at the block's exponent
/// and its values at the run's exponent (for the summaries).
pub struct Sealed<'a> {
    pub enc: &'a Encoded,
    pub values: &'a [i64],
}

fn pad_to(bytes: &mut Vec<u8>, align: usize) {
    while !bytes.len().is_multiple_of(align) {
        bytes.push(0);
    }
}

/// A 256-bit two's complement of an `Int` (panics past 255 bits of
/// magnitude: no cumulative of a store reaches it).
pub fn int_to_le256(x: &Int) -> [u8; 32] {
    let limbs = x.magnitude.limbs();
    assert!(limbs[4] == 0 && limbs[5] == 0 && limbs[3] >> 63 == 0, "a cumulative past 255 bits");
    let mut l = [limbs[0], limbs[1], limbs[2], limbs[3]];
    if x.negative && !x.magnitude.is_zero() {
        // Two's complement: invert and add one.
        let mut carry = 1u128;
        for limb in &mut l {
            let v = (!*limb) as u128 + carry;
            *limb = v as u64;
            carry = v >> 64;
        }
    }
    let mut out = [0u8; 32];
    for (i, limb) in l.iter().enumerate() {
        out[i * 8..i * 8 + 8].copy_from_slice(&limb.to_le_bytes());
    }
    out
}

pub fn int_from_le256(b: &[u8]) -> Int {
    let mut l = [0u64; 4];
    for (i, limb) in l.iter_mut().enumerate() {
        *limb = u64::from_le_bytes(b[i * 8..i * 8 + 8].try_into().unwrap());
    }
    let negative = l[3] >> 63 == 1;
    if negative {
        let mut carry = 1u128;
        for limb in &mut l {
            let v = (!*limb) as u128 + carry;
            *limb = v as u64;
            carry = v >> 64;
        }
    }
    let magnitude = Wide::from_limbs([l[0], l[1], l[2], l[3], 0, 0]);
    Int {
        negative: negative && !magnitude.is_zero(),
        magnitude,
    }
}

pub fn summary_to_bytes(s: &BlockSummary) -> [u8; SUMMARY_BYTES] {
    let mut b = [0u8; SUMMARY_BYTES];
    b[0..4].copy_from_slice(&s.n.to_le_bytes());
    b[4..8].copy_from_slice(&s.argmin.to_le_bytes());
    b[8..12].copy_from_slice(&s.argmax.to_le_bytes());
    b[16..24].copy_from_slice(&s.min.to_le_bytes());
    b[24..32].copy_from_slice(&s.max.to_le_bytes());
    b[32..40].copy_from_slice(&s.first.to_le_bytes());
    b[40..48].copy_from_slice(&s.last.to_le_bytes());
    b
}

pub fn summary_from_bytes(b: &[u8]) -> BlockSummary {
    BlockSummary {
        n: u32::from_le_bytes(b[0..4].try_into().unwrap()),
        argmin: u32::from_le_bytes(b[4..8].try_into().unwrap()),
        argmax: u32::from_le_bytes(b[8..12].try_into().unwrap()),
        min: i64::from_le_bytes(b[16..24].try_into().unwrap()),
        max: i64::from_le_bytes(b[24..32].try_into().unwrap()),
        first: i64::from_le_bytes(b[32..40].try_into().unwrap()),
        last: i64::from_le_bytes(b[40..48].try_into().unwrap()),
    }
}

pub fn cumulative_to_bytes(c: &Cumulative) -> [u8; CUMULATIVE_BYTES] {
    let mut b = [0u8; CUMULATIVE_BYTES];
    b[0..8].copy_from_slice(&c.count.to_le_bytes());
    b[8..24].copy_from_slice(&c.sum.to_le_bytes());
    b[24..56].copy_from_slice(&int_to_le256(&c.sum_sq));
    b[56..88].copy_from_slice(&int_to_le256(&c.sum_kx));
    b
}

pub fn cumulative_from_bytes(b: &[u8]) -> Cumulative {
    Cumulative {
        count: u64::from_le_bytes(b[0..8].try_into().unwrap()),
        sum: i128::from_le_bytes(b[8..24].try_into().unwrap()),
        sum_sq: int_from_le256(&b[24..56]),
        sum_kx: int_from_le256(&b[56..88]),
    }
}

fn exception_to_bytes(x: &Exception) -> [u8; EXCEPTION_BYTES] {
    let mut b = [0u8; EXCEPTION_BYTES];
    let (kind, value) = match *x {
        Exception::Offset { offset, .. } => (0u8, offset),
        Exception::Raw { bits, .. } => (1u8, bits),
    };
    b[0..2].copy_from_slice(&x.position().to_le_bytes());
    b[2] = kind;
    b[8..16].copy_from_slice(&value.to_le_bytes());
    b
}

fn exception_from_bytes(b: &[u8]) -> Exception {
    let position = u16::from_le_bytes(b[0..2].try_into().unwrap());
    let value = u64::from_le_bytes(b[8..16].try_into().unwrap());
    match b[2] {
        0 => Exception::Offset { position, offset: value },
        _ => Exception::Raw { position, bits: value },
    }
}

/// Writes a segment: runs appended in instrument order, then `finish`.
pub struct Writer {
    bytes: Vec<u8>,
    runs: Vec<Run>,
}

impl Default for Writer {
    fn default() -> Self {
        Self::new()
    }
}

impl Writer {
    pub fn new() -> Writer {
        let mut bytes = Vec::with_capacity(1 << 20);
        bytes.extend_from_slice(MAGIC);
        bytes.extend_from_slice(&abt_codec::CODEC_VERSION.to_le_bytes());
        bytes.extend_from_slice(&0u16.to_le_bytes());
        bytes.extend_from_slice(&0u32.to_le_bytes());
        Writer { bytes, runs: Vec::new() }
    }

    /// Bytes written so far (the rotation check).
    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.runs.is_empty()
    }

    pub fn runs(&self) -> &[Run] {
        &self.runs
    }

    /// Appends an instrument's run: its blocks from `first_block` on, with
    /// the cumulative totals `before` the first (zero for block 0) and
    /// `k0` the row index of its first row.
    pub fn run(&mut self, instrument: u32, first_block: u32, exponent: u8, blocks: &[Sealed<'_>], before: &Cumulative, k0: u64) -> Run {
        assert!(!blocks.is_empty());
        let n = blocks.len();
        // Headers, exceptions offsets filled as the lists are laid out.
        let mut headers: Vec<Header> = blocks.iter().map(|b| Header::of(b.enc, false)).collect();
        let mut exceptions = Vec::new();
        for (h, b) in headers.iter_mut().zip(blocks) {
            if !b.enc.exceptions.is_empty() {
                h.exceptions = 1 + exceptions.len() as u32;
                exceptions.extend_from_slice(&(b.enc.exceptions.len() as u32).to_le_bytes());
                for x in &b.enc.exceptions {
                    exceptions.extend_from_slice(&exception_to_bytes(x));
                }
            }
        }
        pad_to(&mut self.bytes, ALIGN);
        let headers_off = self.bytes.len() as u64;
        for h in &headers {
            self.bytes.extend_from_slice(&h.to_bytes());
        }
        pad_to(&mut self.bytes, ALIGN);
        let words_off = self.bytes.len() as u64;
        for b in blocks {
            pad_to(&mut self.bytes, ALIGN);
            let packed = pack(&b.enc.offsets, b.enc.width);
            self.bytes.extend_from_slice(&packed.to_bytes());
        }
        pad_to(&mut self.bytes, ALIGN);
        let exceptions_off = self.bytes.len() as u64;
        self.bytes.extend_from_slice(&exceptions);
        pad_to(&mut self.bytes, ALIGN);
        let summaries_off = self.bytes.len() as u64;
        for b in blocks {
            self.bytes.extend_from_slice(&summary_to_bytes(&BlockSummary::of(b.values)));
        }
        pad_to(&mut self.bytes, ALIGN);
        let cumulatives_off = self.bytes.len() as u64;
        let mut c = before.clone();
        let mut k = k0;
        self.bytes.extend_from_slice(&cumulative_to_bytes(&c));
        let mut rows = 0u64;
        for b in blocks {
            c = c.plus(b.values, k);
            k += b.values.len() as u64;
            rows += b.values.len() as u64;
            self.bytes.extend_from_slice(&cumulative_to_bytes(&c));
        }
        let run = Run {
            instrument,
            first_block,
            blocks: n as u32,
            exponent,
            headers: headers_off,
            words: words_off,
            exceptions: exceptions_off,
            summaries: summaries_off,
            cumulatives: cumulatives_off,
            rows,
        };
        self.runs.push(run);
        run
    }

    /// The footer, trailer and hash; the bytes and the segment's hash (hex).
    pub fn finish(mut self) -> (Vec<u8>, String) {
        pad_to(&mut self.bytes, ALIGN);
        let footer_off = self.bytes.len() as u64;
        for r in &self.runs {
            let mut e = [0u8; RUN_ENTRY];
            e[0..4].copy_from_slice(&r.instrument.to_le_bytes());
            e[4..8].copy_from_slice(&r.first_block.to_le_bytes());
            e[8..12].copy_from_slice(&r.blocks.to_le_bytes());
            e[12] = r.exponent;
            e[16..24].copy_from_slice(&r.headers.to_le_bytes());
            e[24..32].copy_from_slice(&r.words.to_le_bytes());
            e[32..40].copy_from_slice(&r.exceptions.to_le_bytes());
            e[40..48].copy_from_slice(&r.summaries.to_le_bytes());
            e[48..56].copy_from_slice(&r.cumulatives.to_le_bytes());
            e[56..64].copy_from_slice(&r.rows.to_le_bytes());
            self.bytes.extend_from_slice(&e);
        }
        self.bytes.extend_from_slice(&(self.runs.len() as u64).to_le_bytes());
        self.bytes.extend_from_slice(&footer_off.to_le_bytes());
        let hash = blake3::hash(&self.bytes);
        self.bytes.extend_from_slice(hash.as_bytes());
        self.bytes.extend_from_slice(TRAILER_MAGIC);
        (self.bytes, hash.to_hex().to_string())
    }
}

/// A segment's bytes, parsed: the runs and the accessors of their blocks.
pub struct Segment<'a> {
    pub bytes: &'a [u8],
    pub runs: Vec<Run>,
}

impl<'a> Segment<'a> {
    /// Parses the trailer and the run table; `verify` also checks the hash
    /// (a sequential read of the whole file).
    pub fn parse(bytes: &'a [u8], verify: bool) -> Result<Segment<'a>, DataError> {
        let bad = |what: &str| DataError::Schema(format!("segment: {}", what));
        if bytes.len() < PREAMBLE + TRAILER || &bytes[..8] != MAGIC {
            return Err(bad("not a segment"));
        }
        let version = u16::from_le_bytes(bytes[8..10].try_into().unwrap());
        if version != abt_codec::CODEC_VERSION {
            return Err(bad(&format!("codec version {} (this build reads {})", version, abt_codec::CODEC_VERSION)));
        }
        let t = bytes.len() - TRAILER;
        if &bytes[bytes.len() - 8..] != TRAILER_MAGIC {
            return Err(bad("truncated: no trailer"));
        }
        let runs = u64::from_le_bytes(bytes[t..t + 8].try_into().unwrap()) as usize;
        let footer = u64::from_le_bytes(bytes[t + 8..t + 16].try_into().unwrap()) as usize;
        if footer + runs * RUN_ENTRY != t {
            return Err(bad("the run table does not fit"));
        }
        if verify {
            let hash = blake3::hash(&bytes[..t + 16]);
            if hash.as_bytes() != &bytes[t + 16..t + 48] {
                return Err(bad("hash mismatch"));
            }
        }
        let mut out = Vec::with_capacity(runs);
        for i in 0..runs {
            let e = &bytes[footer + i * RUN_ENTRY..footer + (i + 1) * RUN_ENTRY];
            let r = Run {
                instrument: u32::from_le_bytes(e[0..4].try_into().unwrap()),
                first_block: u32::from_le_bytes(e[4..8].try_into().unwrap()),
                blocks: u32::from_le_bytes(e[8..12].try_into().unwrap()),
                exponent: e[12],
                headers: u64::from_le_bytes(e[16..24].try_into().unwrap()),
                words: u64::from_le_bytes(e[24..32].try_into().unwrap()),
                exceptions: u64::from_le_bytes(e[32..40].try_into().unwrap()),
                summaries: u64::from_le_bytes(e[40..48].try_into().unwrap()),
                cumulatives: u64::from_le_bytes(e[48..56].try_into().unwrap()),
                rows: u64::from_le_bytes(e[56..64].try_into().unwrap()),
            };
            if r.headers as usize + r.blocks as usize * HEADER_BYTES > footer || r.cumulatives as usize + (r.blocks as usize + 1) * CUMULATIVE_BYTES > footer {
                return Err(bad("a run points past the footer"));
            }
            out.push(r);
        }
        Ok(Segment { bytes, runs: out })
    }

    pub fn header(&self, run: &Run, b: usize) -> Header {
        let at = run.headers as usize + b * HEADER_BYTES;
        Header::from_bytes(&self.bytes[at..at + HEADER_BYTES])
    }

    /// The offset of block `b`'s words: each block's words are 64-byte
    /// aligned, in order.
    fn words_at(&self, run: &Run, b: usize) -> usize {
        let mut at = run.words as usize;
        for i in 0..b {
            let h = self.header(run, i);
            at = at.div_ceil(ALIGN) * ALIGN + h.words_bytes();
        }
        at.div_ceil(ALIGN) * ALIGN
    }

    pub fn words(&self, run: &Run, b: usize) -> &'a [u8] {
        let h = self.header(run, b);
        let at = self.words_at(run, b);
        &self.bytes[at..at + h.words_bytes()]
    }

    pub fn exceptions(&self, run: &Run, b: usize) -> Vec<Exception> {
        let h = self.header(run, b);
        if h.exceptions == 0 {
            return Vec::new();
        }
        let at = run.exceptions as usize + (h.exceptions - 1) as usize;
        let n = u32::from_le_bytes(self.bytes[at..at + 4].try_into().unwrap()) as usize;
        (0..n)
            .map(|i| exception_from_bytes(&self.bytes[at + 4 + i * EXCEPTION_BYTES..at + 4 + (i + 1) * EXCEPTION_BYTES]))
            .collect()
    }

    pub fn summary(&self, run: &Run, b: usize) -> BlockSummary {
        let at = run.summaries as usize + b * SUMMARY_BYTES;
        summary_from_bytes(&self.bytes[at..at + SUMMARY_BYTES])
    }

    /// The totals before block `boundary` of the run (0 to `blocks`).
    pub fn cumulative(&self, run: &Run, boundary: usize) -> Cumulative {
        let at = run.cumulatives as usize + boundary * CUMULATIVE_BYTES;
        cumulative_from_bytes(&self.bytes[at..at + CUMULATIVE_BYTES])
    }

    /// The block's packed words and exceptions, as the codec reads them.
    pub fn packed(&self, run: &Run, b: usize) -> (Header, Packed, Vec<Exception>) {
        let h = self.header(run, b);
        (h, Packed::from_bytes(h.width, self.words(run, b)), self.exceptions(run, b))
    }

    /// The block's values as integers at the run's exponent (raw
    /// exceptions, values off any grid, as 0 with the header's `raw` flag
    /// telling the reader so).
    pub fn integers(&self, run: &Run, b: usize) -> Vec<i64> {
        let (h, packed, exceptions) = self.packed(run, b);
        let mut out = [0u64; BLOCK];
        unpack(&packed, &mut out);
        abt_codec::block::patch(&exceptions, 0, BLOCK, &mut out);
        let scale = abt_agg::exact::pow10((run.exponent as i32 - h.e as i32).max(0) as u32) as i64;
        (0..h.n as usize).map(|j| h.base.wrapping_add(h.g.wrapping_mul(out[j]) as i64).wrapping_mul(scale)).collect()
    }

    /// The block's values as the source wrote them (Proposition 3.2).
    pub fn values(&self, run: &Run, b: usize) -> Vec<f64> {
        let (h, packed, exceptions) = self.packed(run, b);
        let mut out = [0u64; BLOCK];
        unpack(&packed, &mut out);
        abt_codec::block::patch(&exceptions, 0, BLOCK, &mut out);
        let mut raw = exceptions
            .iter()
            .filter_map(|x| if let Exception::Raw { position, bits } = x { Some((*position as usize, *bits)) } else { None })
            .peekable();
        (0..h.n as usize)
            .map(|j| {
                if raw.peek().is_some_and(|(p, _)| *p == j) {
                    let (_, bits) = raw.next().unwrap();
                    return f64::from_bits(bits);
                }
                abt_codec::tick::decode_value(h.base, h.g, h.e as u8, h.recip_exact(), h.f32(), out[j])
            })
            .collect()
    }

    /// The block's hash: BLAKE3 over its header, words and exceptions.
    pub fn block_hash(&self, run: &Run, b: usize) -> blake3::Hash {
        let mut h = blake3::Hasher::new();
        let at = run.headers as usize + b * HEADER_BYTES;
        h.update(&self.bytes[at..at + HEADER_BYTES]);
        h.update(self.words(run, b));
        let hd = self.header(run, b);
        if hd.exceptions != 0 {
            let at = run.exceptions as usize + (hd.exceptions - 1) as usize;
            let n = u32::from_le_bytes(self.bytes[at..at + 4].try_into().unwrap()) as usize;
            h.update(&self.bytes[at..at + 4 + n * EXCEPTION_BYTES]);
        }
        h.finalize()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use abt_codec::tick::{encode, encode_integers, Source};

    #[test]
    fn a_segment_round_trips_its_blocks_summaries_and_cumulatives() {
        let prices: Vec<f64> = (0..2500).map(|i| 100.0 + 0.25 * ((i * 7) % 40) as f64).collect();
        let counts: Vec<i64> = (0..1500).map(|i| 1_000 + 13 * (i % 50)).collect();
        let mut w = Writer::new();
        // Instrument 0: prices in three blocks at exponent 2.
        let encs: Vec<Encoded> = prices.chunks(BLOCK).map(|c| encode(c, Source::F64)).collect();
        let values: Vec<Vec<i64>> = encs.iter().map(|e| e.integers().unwrap()).collect();
        let blocks: Vec<Sealed> = encs.iter().zip(&values).map(|(enc, values)| Sealed { enc, values }).collect();
        let r0 = w.run(0, 0, 2, &blocks, &Cumulative::ZERO, 0);
        // Instrument 3: counts in two blocks at exponent 0.
        let encs2: Vec<Encoded> = counts.chunks(BLOCK).map(encode_integers).collect();
        let values2: Vec<Vec<i64>> = encs2.iter().map(|e| e.integers().unwrap()).collect();
        let blocks2: Vec<Sealed> = encs2.iter().zip(&values2).map(|(enc, values)| Sealed { enc, values }).collect();
        let r1 = w.run(3, 0, 0, &blocks2, &Cumulative::ZERO, 0);
        let (bytes, hash) = w.finish();
        let s = Segment::parse(&bytes, true).unwrap();
        assert_eq!(s.runs, vec![r0, r1]);
        assert_eq!(hash.len(), 64);
        for (b, c) in prices.chunks(BLOCK).enumerate() {
            assert_eq!(s.values(&s.runs[0], b), c);
            assert_eq!(s.integers(&s.runs[0], b), values[b]);
            assert_eq!(s.summary(&s.runs[0], b), BlockSummary::of(&values[b]));
        }
        let cum = abt_codec::summary::cumulatives(values.iter().map(|v| v.as_slice()));
        for (i, c) in cum.iter().enumerate() {
            assert_eq!(&s.cumulative(&s.runs[0], i), c);
        }
        for (b, c) in counts.chunks(BLOCK).enumerate() {
            assert_eq!(s.integers(&s.runs[1], b), c);
        }
        // A corrupted byte fails the hash.
        let mut bad = bytes.clone();
        bad[100] ^= 1;
        assert!(Segment::parse(&bad, true).is_err());
        assert!(Segment::parse(&bad, false).is_ok());
        // Block hashes differ between distinct blocks and equal across copies.
        assert_ne!(s.block_hash(&s.runs[0], 0), s.block_hash(&s.runs[0], 1));
        let s2 = Segment::parse(&bytes, false).unwrap();
        assert_eq!(s.block_hash(&s.runs[0], 2), s2.block_hash(&s2.runs[0], 2));
    }

    #[test]
    fn signed_256_bit_round_trips() {
        for x in [0i128, 1, -1, i128::MAX, i128::MIN, 123_456_789_012_345_678_901_234_567i128, -(1i128 << 100)] {
            let i = Int::from_i128(x);
            assert_eq!(int_from_le256(&int_to_le256(&i)), i, "{}", x);
        }
        let big = Int::product(i128::MAX, 1i128 << 100);
        assert_eq!(int_from_le256(&int_to_le256(&big)), big);
        let neg = big.neg();
        assert_eq!(int_from_le256(&int_to_le256(&neg)), neg);
    }
}
