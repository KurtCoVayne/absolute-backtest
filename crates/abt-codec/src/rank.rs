//! Presence bitmaps with rank and select (ST-5.4): per instrument and
//! level, a bit per label of the level within the instrument's alive
//! interval, and a directory in the rank9 form (Jacobson 1989; Vigna 2008):
//! a cumulative popcount every 512 bits and, packed in one word per block,
//! a 9-bit relative count for each of its 64-bit words but the first, about
//! 25 % over the bits. `rank(i)` (the set bits before `i`: the row of a
//! label) is O(1); `select(k)` (the position of the `k`-th set bit: the
//! label of a row) is a binary search over the blocks and a scan of one.

/// Bits per directory block.
const BLOCK_BITS: usize = 512;
const WORDS: usize = BLOCK_BITS / 64;

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Bitmap {
    bits: Vec<u64>,
    len: usize,
    /// Per 512-bit block: the set bits before it.
    cumulative: Vec<u64>,
    /// Per block: the set bits before each of its words 1 to 7, 9 bits each.
    relative: Vec<u64>,
    ones: u64,
}

impl Bitmap {
    pub fn new() -> Bitmap {
        Bitmap::default()
    }

    /// A bitmap of `len` bits, the set ones given in any order.
    pub fn of(len: usize, set: impl IntoIterator<Item = usize>) -> Bitmap {
        let mut b = Bitmap {
            bits: vec![0; len.div_ceil(64)],
            len,
            cumulative: Vec::new(),
            relative: Vec::new(),
            ones: 0,
        };
        for i in set {
            assert!(i < len);
            b.bits[i / 64] |= 1 << (i % 64);
        }
        b.rebuild(0);
        b
    }

    /// A bitmap of `len` bits from its stored words (bit `i` is bit `i mod
    /// 64` of word `i div 64`); the directory is rebuilt.
    pub fn from_words(len: usize, words: &[u64]) -> Bitmap {
        assert!(words.len() >= len.div_ceil(64));
        let mut b = Bitmap {
            bits: words[..len.div_ceil(64)].to_vec(),
            len,
            cumulative: Vec::new(),
            relative: Vec::new(),
            ones: 0,
        };
        if !len.is_multiple_of(64) {
            if let Some(last) = b.bits.last_mut() {
                *last &= (1u64 << (len % 64)) - 1;
            }
        }
        b.rebuild(0);
        b
    }

    /// The stored words.
    pub fn words(&self) -> &[u64] {
        &self.bits
    }

    /// The bits as a slice of bools (tests).
    pub fn from_bools(bits: &[bool]) -> Bitmap {
        Bitmap::of(bits.len(), bits.iter().enumerate().filter(|(_, &b)| b).map(|(i, _)| i))
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The number of set bits.
    pub fn ones(&self) -> u64 {
        self.ones
    }

    pub fn get(&self, i: usize) -> bool {
        assert!(i < self.len);
        (self.bits[i / 64] >> (i % 64)) & 1 == 1
    }

    /// Rebuilds the directory from block `from` on.
    fn rebuild(&mut self, from: usize) {
        let blocks = self.bits.len().div_ceil(WORDS);
        self.cumulative.truncate(from);
        self.relative.truncate(from);
        let mut count: u64 = if from == 0 { 0 } else { self.cumulative[from - 1] + self.block_ones(from - 1) };
        for b in from..blocks {
            self.cumulative.push(count);
            let mut packed = 0u64;
            let mut within = 0u64;
            for w in 0..WORDS {
                let idx = b * WORDS + w;
                if w > 0 {
                    packed |= (within & 0x1FF) << (9 * (w - 1));
                }
                if idx < self.bits.len() {
                    within += self.bits[idx].count_ones() as u64;
                }
            }
            self.relative.push(packed);
            count += within;
        }
        self.ones = count;
    }

    fn block_ones(&self, b: usize) -> u64 {
        (b * WORDS..((b + 1) * WORDS).min(self.bits.len())).map(|i| self.bits[i].count_ones() as u64).sum()
    }

    /// The set bits in `[0, i)`.
    pub fn rank(&self, i: usize) -> u64 {
        assert!(i <= self.len);
        if i == 0 {
            return 0;
        }
        let (b, w, bit) = (i / BLOCK_BITS, (i / 64) % WORDS, i % 64);
        if i / 64 >= self.bits.len() {
            return self.ones;
        }
        let rel = if w == 0 { 0 } else { (self.relative[b] >> (9 * (w - 1))) & 0x1FF };
        let partial = if bit == 0 { 0 } else { (self.bits[i / 64] & ((1u64 << bit) - 1)).count_ones() as u64 };
        self.cumulative[b] + rel + partial
    }

    /// The clear bits in `[0, i)`.
    pub fn rank0(&self, i: usize) -> u64 {
        i as u64 - self.rank(i)
    }

    /// The position of the `k`-th set bit (`k` from 0), if it exists.
    pub fn select(&self, k: u64) -> Option<usize> {
        if k >= self.ones {
            return None;
        }
        // The last block whose cumulative count is at most k.
        let b = self.cumulative.partition_point(|&c| c <= k) - 1;
        let mut remaining = k - self.cumulative[b];
        for w in 0..WORDS {
            let idx = b * WORDS + w;
            if idx >= self.bits.len() {
                break;
            }
            let ones = self.bits[idx].count_ones() as u64;
            if remaining < ones {
                return Some(idx * 64 + select_in_word(self.bits[idx], remaining));
            }
            remaining -= ones;
        }
        None
    }

    /// The position of the `k`-th clear bit.
    pub fn select0(&self, k: u64) -> Option<usize> {
        if k >= self.len as u64 - self.ones {
            return None;
        }
        let zeros_before = |b: usize| (b * BLOCK_BITS) as u64 - self.cumulative[b];
        let blocks = self.cumulative.len();
        let (mut lo, mut hi) = (0usize, blocks);
        while hi - lo > 1 {
            let mid = (lo + hi) / 2;
            if zeros_before(mid) <= k {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        let mut remaining = k - zeros_before(lo);
        for w in 0..WORDS {
            let idx = lo * WORDS + w;
            if idx >= self.bits.len() {
                break;
            }
            let valid = (self.len - idx * 64).min(64);
            let word = !self.bits[idx] & if valid == 64 { u64::MAX } else { (1u64 << valid) - 1 };
            let zeros = word.count_ones() as u64;
            if remaining < zeros {
                return Some(idx * 64 + select_in_word(word, remaining));
            }
            remaining -= zeros;
        }
        None
    }

    /// Appends a bit (ST-8.4: the label's bit set, the directory's counters
    /// updated at the word and block boundaries).
    pub fn push(&mut self, set: bool) {
        let i = self.len;
        self.len += 1;
        if i.is_multiple_of(64) {
            self.bits.push(0);
        }
        if set {
            self.bits[i / 64] |= 1 << (i % 64);
        }
        let b = i / BLOCK_BITS;
        if i.is_multiple_of(BLOCK_BITS) {
            // A new block: its cumulative count is the total so far.
            self.cumulative.push(self.ones);
            self.relative.push(0);
        }
        if set {
            self.ones += 1;
        }
        // The relative counts of the words after this one within the block
        // grow by one when a set bit is appended; only later words exist
        // once they are pushed, so update the packed counts for words > w.
        let w = (i / 64) % WORDS;
        if set {
            for later in (w + 1)..WORDS {
                let shift = 9 * (later - 1);
                let cur = (self.relative[b] >> shift) & 0x1FF;
                self.relative[b] = (self.relative[b] & !(0x1FFu64 << shift)) | ((cur + 1) << shift);
            }
        }
    }

    /// Sets bit `i` (a label within the span, below the length), rebuilding
    /// the directory from its block.
    pub fn set(&mut self, i: usize) {
        assert!(i < self.len);
        if !self.get(i) {
            self.bits[i / 64] |= 1 << (i % 64);
            self.rebuild(i / BLOCK_BITS);
        }
    }

    /// The directory's words (its size, for the budget).
    pub fn directory_words(&self) -> usize {
        self.cumulative.len() + self.relative.len()
    }
}

/// The position of the `k`-th set bit of a word.
fn select_in_word(mut word: u64, mut k: u64) -> usize {
    loop {
        let t = word.trailing_zeros() as usize;
        if k == 0 {
            return t;
        }
        word &= word - 1;
        k -= 1;
    }
}
