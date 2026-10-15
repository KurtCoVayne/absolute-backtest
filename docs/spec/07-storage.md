# 07 · Storage: the hot store for Norgate and Databento

Oct 15, 2026 · the resident format, the precomputed structures, the staging pipeline and the append path of the data layer, decided from measurements on the company's own data. Tags and cross-references as in `00-architecture.md`; sections of this document are cited as `ST-n`, and the stages of the staging pipeline as S0 to S6 (ST-7).

This document supersedes, for Norgate and Databento: the `f64` representation of prices and quantities (D-2.1, the [D] after the type table); the resident layout of D-3.1 and D-3.2 (time-major Arrow and Parquet partitions as the in-memory form); and the store structures of X-2.1. Everything else in `01-data.md` stands: the log as the source of truth, versions and bindings, snapshot reads by sequence number, the query forms, the streams and clock barriers, the cache, the resolver. ORATS is out of scope here (ST-11).

---

## 1. The result in one paragraph

Every Norgate and Databento value is held hot, about 3.3 billion values in roughly 6.7 to 7.0 GB with every precomputed structure, inside the 16 GB machine with room for the kernel and the query layer. Prices and quantities are exact integer ticks: each block of 1,024 values of one instrument carries a quantum found from the data (a decimal exponent times a greatest common divisor), so every vendor value converts to an integer and back bit for bit. Blocks are stored instrument-contiguous and lane-interleaved (the FastLanes layout), offsets from a base, with OHLC predicted from the close and rare outliers patched, and a decode-and-sum runs at 0.18 to 0.38 ns a value, faster than scanning the same values uncompressed (0.63 to 0.85 ns as `i64`), because the scan is bandwidth-bound and the packed form is two to six times smaller. Everything that does not depend on a program is precomputed and hot: cumulative block summaries that answer any window, block min and max with sparse tables, the full calendar ladder of bars, presence bitmaps with rank directories, the catalog structures, standing event aggregates, and wavelet matrices for order statistics. Data enters through a seven-stage staging pipeline that verifies every block by decoding it, publishes versions atomically and shares unchanged blocks between versions; appends are written in place into each instrument's tail block and become visible by sequence number, so a reader bound to an earlier sequence never sees them, and revisions of history become new versions by copy-on-write.

---

## 2. Evidence

All figures are measured on the company's data in S3 (Oct 15, 2026): NDLake `bars_by_date` for 1995 and 2024 (6.8M equity rows), Databento `ohlcv-1m` for ES, ZN, ZT, CL, GC and 6J on 2025-03-03 and 2025-03-04 with their definitions, on a 2-core 2.8 GHz VM. The scripts and their outputs are in `scripts/study/encoding/` and reproduce every number below. Sizes for the full history scale these bytes per value by row counts estimated from the S3 sizes (NDLake 4.2 GB, about 148M rows; Databento 6.9 GB in 117,677 files, about 515M rows), within ±25 %.

### 2.1 Exactness

| Source | Stored as | What the values are | Round trip through the integer form |
| --- | --- | --- | --- |
| NDLake prices | `double`, every value exactly an `f32` | 1995: binary fractions (29 % need 3 decimals: eighths); 2024: 65 % at 2 decimals, the rest at 3 to 6 (sub-penny names) | 100 %, zero exceptions |
| NDLake volume | `int64` | integers | — |
| Databento prices | `double` | 100 % on each contract's `min_price_increment`; not `f32` (3.9 % of CL values and none of 6J are exact `f32`) | 100 % |
| Databento volume | `uint64` | integers | — |

### 2.2 The quantum

| Finding | Measurement |
| --- | --- |
| One exponent per column is set by its tail | NDLake 2024 close: 0.3 % of values need 6 decimals, so a column-wide `DECIMAL(·,6)` takes 2.84 B/value in Parquet, more than `f32` (2.60) |
| The exponent per block | the same column at 1.49 B/value packed |
| The block GCD finds binary-fraction ticks | NDLake 1995 close 1.51 → 0.80 B/value (quantum 1/8); ZN 2.31 → 0.54 (1/64); ZT 2.93 → 0.55 (1/256); ES 1.39 → 0.83 (0.25) |
| Definitions do not cover spreads | 76 % of CL rows (calendar spreads) have no definition row; the GCD finds their tick |

### 2.3 Prediction and layout

| Finding | Measurement (bytes per value) |
| --- | --- |
| OHLC from the close (high above max(open, close), low below min(open, close), open as the gap from the previous close) | NDLake 2024 1.54 → 1.39; 1995 0.83 → 0.67; ES 0.90 → 0.70; CL 0.63 → 0.45; ZN 0.59 → 0.38; GC 0.80 → 0.60 |
| Volume: offsets with patched exceptions beat deltas | NDLake 2024 2.57 (deltas 2.79); CL 1.01 (1.24) |
| Instrument-contiguous against the vendor's date-major order | NDLake 2024 close 1.49 against 3.55 |

### 2.4 Speed

| Operation (33.5M values unless noted) | ns per value |
| --- | --- |
| Lane-interleaved decode + exact sum, widths 5 / 8 / 11 / 14 / 20 / 27 | 0.18 / 0.22 / 0.26 / 0.26 / 0.33 / 0.38 |
| Uncompressed `u32` scan | 0.37 to 0.46 |
| Uncompressed `i64` scan | 0.63 to 0.85 |
| Uncompressed `f64` sum in canonical order (`two_pass`) | 1.33 to 1.42 |
| Horizontal (non-interleaved) bit-packing, real columns, fused | 1.3 to 1.8 |
| Exact `f32` rebuilt with a division (real columns) | 2.6 to 2.9 |
| Rolling 252-row mean, sliding state (real NDLake 2024, 4.19M values) | 1.78 per output |
| Encode a block: minimum, GCD, width, pack | 10 to 11 |

| Random window sum, width 11 | Cumulative block sums + the shorter edge decoded | Scanning raw `u32` |
| --- | --- | --- |
| length 5 to 255 | 598 ns | 57 ns |
| length 250 to 2,250 | 759 ns | 662 ns |
| length 2,000 to 22,000 | 728 ns | 4,596 ns |

### 2.5 Rejected alternatives

| Alternative | Why not |
| --- | --- |
| Uncompressed resident columns (`i64` ticks and `f64` values, two layouts) | about 26 GB for the values alone, and slower than the packed form on every scan (2.4) |
| pcodec (distribution-aware entropy coding) in memory | smallest (about 2.5 GB in total; 1.24 B/value for NDLake 2024 close, 0.32 for ZN), but decodes at 2.5 to 5.5 ns/value, ten to twenty times slower than the lane-interleaved form; kept for the cold tier only (S6 in ST-7) |
| Deltas instead of offsets | about 10 % smaller, but decoding needs a sequential running sum (1.23 against 0.99 ns/value fused); offsets keep every lane independent |
| Rebuilding vendor greeks or prices from a model | not applicable to Norgate and Databento; for ORATS the model columns do not follow Black-Scholes (ST-11) |

---

## 3. Values

**Definition 3.1 (tick column).** A tick column is a column whose values are exact rationals on a grid. For each block `b` it stores a base `β_b` (an `i64`), a quantum `q_b = g_b · 10^(−e_b)` with `g_b ≥ 1` an integer and `0 ≤ e_b ≤ 9`, a width `W_b`, and offsets `u_i ∈ [0, 2^{W_b})`, so that the value is `x_i = (β_b + g_b · u_i) · 10^(−e_b)`.

**Encoding.** For a block of values `x_1 … x_n` (n ≤ 1,024) of one instrument: `e_b` is the smallest exponent at which every value round-trips (for an `f32` source, `f32(round(x · 10^e) / 10^e) = x`; for an `f64` source, `round(x · 10^e) / 10^e = x`); the integers `i_k = round(x_k · 10^{e_b})`; `β_b = min i_k`; `g_b = gcd(i_k − β_b)` (1 when all equal, with `W_b = 0`); `u_k = (i_k − β_b) / g_b`; `W_b` the bit length of `max u_k`. The encoder then decodes the block with the reader's formula and compares bit for bit with the source; a value that does not reproduce is a block exception (ST-4.4). None was observed.

**Proposition 3.2 [N] (lossless).** Decoding any block returns the source values bit for bit. *Proof.* By construction: the encoder accepts a block only after decoding it with the reader's formula and comparing every value with its source; a mismatch is stored as an exception with its exact bits. ∎

**Floats on read.** Prices are never stored as floats. When an expression needs one, a block is rebuilt in L1: `f64 = (β + g·u) / 10^e`, correctly rounded; for an `f32` source, that `f64` rounded to `f32` gives the vendor's bits. The encoder also tests the multiply form `(β + g·u) · 10^(−e)` (a precomputed reciprocal) on every value of the block and sets the block's `recip_exact` flag when it reproduces the same bits; readers use the multiply when the flag is set and the division otherwise. Databento values have no `f32` view: their view is the exact tick value as `f64`.

**Proposition 3.3 [N] (exact sums).** For any set of rows of one or several blocks, `Σ x` is computed exactly. *Proof.* Within a block, `Σ u` of at most 1,024 offsets of at most `W ≤ 52` bits fits an `i64`; the block's contribution in the instrument's finest exponent `e* = max e_b` is `(n·β_b + g_b·Σu)·10^{e* − e_b}`, an integer, accumulated in `i128`. The result is the exact rational `S·10^(−e*)`; its `f64` is one correctly rounded division. `Σx²` and `Σk·x` follow the same pattern in `i128` (and in 256-bit arithmetic for windows over about 10^5 rows of large prices). ∎ This is the `exact` numerics mode; `two_pass` remains for the parity books (MW14, R8L), which sum the decoded `f64` values sequentially in canonical order.

---

## 4. Blocks

### 4.1 Organisation

For each (relation, column, calendar level) the store holds, per instrument, one contiguous run of blocks covering the instrument's whole history in time order. A block holds 1,024 values; the instrument's last block is its tail (ST-8.3), packed without padding. Instruments are ordered by id; a segment file holds a contiguous range of instruments (ST-7.1).

### 4.2 The lane-interleaved layout

A block of 1,024 offsets is 32 lanes of 32 values: value `j` lives in lane `j mod 32` at row `j div 32`, and row `i` of every lane occupies bits `[i·W, (i+1)·W)` of that lane's `W` words, so the packed block is `32·W` 32-bit words (64-bit words for `W > 32`). Unpacking row `i` is the same shift and mask on 32 adjacent words, which compilers vectorise (8 lanes per AVX2 instruction, 16 with AVX-512, 4 with NEON on the M4). A single value is extracted with one or two word reads. Reference: Afroozeh and Boncz, *The FastLanes Compression Layout*, VLDB 2023.

### 4.3 Header

32 bytes per block: `base: i64`, `g: u64`, `e: i8`, `W: u8`, `n: u16`, `flags: u16` (`recip_exact`, `patched`, `predicted`), `exceptions: u32` (offset into the block's exception list, 0 when none), padding to 32. Headers are stored apart from the packed words (a header array per column), so a scan reads headers sequentially.

### 4.4 Patched exceptions

Per block the width is `W* = argmin_W [ n·W + 74·#{u > 2^W − 1} ]`, 74 bits being the cost of an exception (its 10-bit position and 64-bit value). Values above the width are stored in the block's exception list and written back after unpacking. Volume blocks use this routinely (a rebalance day or a 10× spike widens one block otherwise); price blocks rarely need it.

### 4.5 OHLC prediction

A bar relation with open, high, low and close stores four tick columns per instrument: `C`; `H′ = H − max(O, C)`; `L′ = min(O, C) − L`; `O′ = O − C_prev`, with `C_prev` the previous row's close of the same instrument and, for the instrument's first row, `O′ = 0` and `O` stored in the instrument's directory entry (ST-4.8). The residuals are computed on integers at the finest exponent of the four columns over the block's rows, so they are exact; each residual column then has its own block headers, and its quantum is found from its own values (the GCD of `H′` is usually the close's tick). Decoding is `C`, then `O = O′ + C_prev`, then `H = H′ + max(O, C)` and `L = min(O, C) − L′`, element-wise on decoded blocks after aligning exponents (an integer multiplication by `10^{e* − e}`, exact). `H′ ≥ 0` and `L′ ≥ 0` hold on vendor data; a row violating them (a vendor error) is stored as an exception with its raw value and reported by S1.

### 4.6 Other columns

Volume and open interest are tick columns with `e = 0`. Sparse columns (Norgate `div_cash`, `split_ratio`; corporate actions) are not dense columns but per-instrument event lists (ST-5.5). Constant columns per instrument (a security's exchange, a contract's multiplier) live in the registry.

### 4.7 Time

No per-row timestamp is stored. Each instrument has, per calendar level, a presence bitmap over the level's labels (ST-5.4), and the `k`-th set bit is row `k`. Receipt time follows the source's convention, recorded per version: Norgate bars are available at the bar's close plus the version's processing delay; Databento historical `ohlcv-1m` carries no `ts_recv`, so receipt is `ts_event` plus the configured delay (B-3, D-7.7). Rows appended live carry their own receipt time in the append log (ST-8.2), not in blocks.

### 4.8 Directory

Per (relation, column, level, instrument): the first block id, the block count, the first and last label indices, the tail's location, and for bar relations the first row's open (ST-4.5). With the per-column header array, locating the block of row `k` is `first + k div 1024`, O(1).

---

## 5. Precomputed structures

Everything that is a function of the stored data alone, independent of any program, is precomputed at staging and kept hot. What depends on a program's parameters is answered from these structures or recomputed in run-scoped memory (ST-5.8).

| Structure | Granularity | Answers | Size (both sources, all levels) |
| --- | --- | --- | --- |
| Block summary: `n`, min, max, the first time of each, first, last | per block | zone-map pruning; block-aligned min, max, first, last | in the 0.55 GB below |
| Cumulative summary: `count`, `Σx`, `Σx²`, `Σk·x` exact in the instrument's finest exponent | per instrument at every block boundary | any window's moments: whole blocks from two cumulative entries, the edges decoded | 0.55 GB together with the block summaries and sparse tables |
| Sparse table over block min and max | per instrument | range min and max over whole blocks in O(1) | (above) |
| Calendar ladder (ST-5.3) | per instrument per level | any bucket of a standard calendar in O(1); coarser levels from finer | 1.0 GB |
| Presence bitmap + rank directory per level (ST-5.4) | per instrument | (instrument, label) → row in O(1); which instruments traded at a label | 0.3 GB |
| Catalog structures (ST-5.5) | per instrument, per index, per root | identity, symbols, actions, membership, delistings, classification, calendars, contract specs, roll map | 0.1 GB |
| Standing event aggregates (ST-5.6) | per instrument | adjustment factors as step functions | (in the catalog) |
| Wavelet matrices (ST-5.7) | per instrument, close and volume, per level | range median, quantile, k-th in O(log σ) | 0.75 GB |

### 5.1 Windows from cumulative summaries

The sum over rows `[a, b)` of an instrument is `C(b) − C(a)`, where `C(k)` is the cumulative total at the start of `k`'s block plus the sum of the rows of that block before `k`. The in-block part decodes the shorter side only: the prefix when it is at most 512 values (unpacking only the rows it spans), otherwise the block total minus the suffix. A window of at most two blocks is decoded directly instead. Cost (ST-2.4): about 0.6 to 0.75 µs per random window whatever its length, dominated by the two cache misses, against a scan that grows with the length (4.6 µs at 2,000 to 22,000 rows). Sequential rolling windows (the kernel's and the aggregate pass's case) use sliding states over decoded blocks, 1.8 ns per output, and need no lookup at all.

**Proposition 5.1 [N] (exact windows).** Window moments computed from cumulative summaries equal those of a direct reduction of the window, exactly. *Proof.* Proposition 3.3: every term is an exact integer in the instrument's finest exponent, and subtraction of exact integers is exact. ∎ Order statistics have no cumulative form; they use ST-5.7.

### 5.2 Min and max over ranges

A range's minimum is the minimum of the sparse-table answer over its whole blocks and the two edge minima, the edges decoded as in ST-5.1. The sparse table has `⌈log₂ m⌉` rows over an instrument's `m` blocks; instruments have few blocks per level, so it is small.

### 5.3 The calendar ladder

| Source | Base | Levels precomputed |
| --- | --- | --- |
| Norgate | day | week, month, quarter, year |
| Databento | 1 minute | 5, 15 and 30 minutes, 1 hour, session (per root, from the session table), day, week, month |

Each level is a bar relation (open, high, low, close, volume, and the number of base rows in the bucket) in the block format of ST-4 with its own summaries, presence and wavelets. A level is built from the finest level it coarsens (week and month from day; quarter from month; 15 minutes from 5), never from the base when a finer built level exists.

**Proposition 5.2 [N] (exact ladder).** Every precomputed bar equals the bar computed from the base rows of its bucket. *Proof.* Open, high, low and close are first, max, min and last, which are exactly associative (they select one input value); volume and the row count are sums of integers, exact by Proposition 3.3; a coarser bucket is a union of finer buckets because the ladder's calendars are ordered by refinement (V-3.3, Prop. 3.3), so building from a finer level is a regrouping of the same inputs. ∎ Calendars outside the ladder are built on demand from the nearest finer level (ST-5.8).

### 5.4 Presence and rank

Per instrument and level, a bitmap over the level's labels within the instrument's alive interval marks the labels at which it has a row. A rank directory (a cumulative popcount every 512 bits plus a 9-bit relative count every 64 bits, about 25 % overhead; Jacobson 1989; Vigna 2008, rank9) answers "the row of label `ℓ`" in O(1), and select answers "the label of row `k`". The minute presence of futures is the larger part (about 0.23 GB before the directory); equity day presence is about 20 MB.

### 5.5 Catalog structures

| Relation | Structure |
| --- | --- |
| Security master, vendor ids, tickers | interval indexes keyed by (vendor, key) and by symbol, binary search on time (D-2.4) |
| Corporate actions (splits, dividends, delistings) | per-instrument event lists in time order |
| Index membership (52 indexes) | the vendor's intervals (191,143 rows) plus, per index, a compressed bitmap of member instruments per day (roaring bitmaps; Lemire et al. 2016), a few MB |
| Classification, fundamentals | per-instrument event lists |
| Trading calendars, futures sessions | per calendar a sorted label array and the session table |
| Futures definitions | per contract: tick, multiplier, expiry, activation, unit; spreads inherit their legs' tick and are verified by the block GCD |
| Continuous roll map | per root, the contract held at each date with its additive adjustment |

### 5.6 Standing event aggregates

Some program-independent results are needed by every equity program and are pure arithmetic over catalog events: the cumulative split factor and the cumulative total-return factor per instrument. They are declared by the kernel's `catalog` library as standing aggregate requests (cumulative product of a column of an event relation per entity, D-6.1), which the data layer evaluates at staging and stores as step functions: one entry per event. An adjusted price or a total-return index value is then the stored tick value times the step function's current factor, one multiply on read. The semantics stay in the request, which the kernel authors; the data layer computes a product (A-D6).

### 5.7 Order statistics

Range median, quantile and k-th smallest have no cumulative form. They are answered by a wavelet matrix per (instrument, column, level) over the rank-reduced offsets, in O(log σ), σ the number of distinct values of the instrument's column (Claude, Navarro and Ordóñez 2015; Gagie, Puglisi and Turpin 2009 for range quantiles). A matrix costs about `⌈log₂ σ⌉ · 1.25` bits a value (14 to 17 bits for a price or a volume over an instrument's history) plus the sorted distinct values. Built at staging for close and volume at Norgate's day level and coarser and at Databento's 1-hour level and coarser (about 0.75 GB); open, high and low, and finer futures levels, are built on first demand (about 0.35 GB a column at Norgate's day level) and kept for the life of the version. Doing all five columns at staging would cost about 1.8 GB; the extra 1 GB stays as headroom in ST-9.

### 5.8 What is not precomputed, and why

| Not precomputed | Because | Answered by |
| --- | --- | --- |
| A program's rolling results (a 252-row mean, a 60-day ADV) | they depend on the program's parameters, there are unboundedly many, and each is a float column that does not pack (8 B per row) | the cumulative summaries (random access) or a sliding state in the aggregate pass (sequential, 1.8 ns per output), in run-scoped memory |
| Running totals per row | they would double or triple the store for a saving of one or two cache misses per window | the cumulative summaries at block boundaries |
| Calendars outside the ladder | unbounded | built on demand from the nearest finer level and cached until the version is evicted |
| Wavelet matrices below 1 hour for futures, and for open, high and low | about 1 GB per column at 5 minutes; order statistics of raw open, high and low are rarely asked | built on first demand |
| Order statistics of derived series (returns, dollar volume, a ratio) | the series is a program's expression | a sliding order-statistic structure in the aggregate pass (O(log w) per output), or a wavelet matrix built in run scope over the derived column |

---

## 6. Query paths

How each form of D-4.1 runs on this format, with its cost.

| Form | Algorithm | Cost |
| --- | --- | --- |
| `point` (instrument, label) | directory → rank → block → lane extract | O(1); about 50 to 100 ns cold |
| `latest` (instrument, ≤ t) | rank of t's label, select the previous set bit, extract | O(1) |
| `series` (instrument, [t0, t1]) | rank both ends, decode the covered rows | 0.18 to 0.38 ns per value plus one miss per block |
| `section` (label) | for each instrument alive at the label (alive intervals), test the presence bit and extract | O(alive): about 50 to 100 µs for 10,000 instruments |
| `scan` with a predicate | block summaries prune whole blocks; survivors decoded and filtered | proportional to surviving blocks |
| `aggregate`, sequential window | decode + sliding state | 1.8 ns per output |
| `aggregate`, random window | cumulative summaries + shorter edge (ST-5.1) | about 0.6 to 0.75 µs |
| `aggregate`, ladder bucket | one row of the level | O(1) |
| range min, max | sparse table + edges | O(1) + edges |
| range median, quantile | wavelet matrix | O(log σ) |
| `subscribe` (replay) | one cursor per live instrument holding its current decoded block; at each label, the instruments alive and present advance | each block decoded once; O(alive) per label |

---

## 7. Staging

Data moves from a vendor to the hot store through seven stages. Each stage's output is immutable; a failure leaves the published version untouched.

| Stage | Input | Output | Invariant checked |
| --- | --- | --- | --- |
| S0 Land | vendor objects: NDLake year files and registry, Databento files listed in `manifest.csv`, the live gateway | the landing log: (source, key, ETag or SHA-256, size, fetched at) | the object's checksum matches the vendor's manifest where one exists (Databento's `manifest.csv` carries SHA-256) |
| S1 Normalise | landed objects | Arrow batches in local staging (NVMe), one per (relation, instrument range), sorted by (instrument, time): ids from the registry, values as exact integers with their source exponent, receipt times | schema conformance and identity (D-8); every value converts exactly (ST-3) or is listed |
| S2 Classify | normalised batches and the current version | per instrument: appends (time after its last row), duplicates (identical to stored), revisions (an existing row with a different value), late rows (inside the tail's span) | duplicates dropped; counts per class reported |
| S3 Build | appends, revisions | appends applied to tails (ST-8); revisions: every affected block re-encoded copy-on-write, its block summaries, the cumulative summaries from that block to the instrument's end, the ladder buckets containing it, presence, wavelets of the affected instrument and level | — |
| S4 Verify | built blocks | a verification report | every new block decodes to its source bit for bit (Prop. 3.2); summaries of new blocks equal a direct recomputation; for a sample of ladder buckets, the bar equals the bar recomputed from base rows (Prop. 5.2); row counts per instrument equal the source's |
| S5 Publish | verified segments | a new version manifest (the list of segment hashes and the sequence high-water), written then renamed into place | atomic: a reader sees the old manifest or the new one; readers holding the old version keep it (pinning, D-3.4) |
| S6 Export and collect | published segments | the cold copy in S3 (pcodec-compressed segments, or Parquet for interoperability); unpinned superseded segments deleted | the cold copy round-trips to the hot form |

### 7.1 Versions and structural sharing

A version is a manifest listing segment files by content hash plus a sequence high-water. A segment holds one (relation, column, level) for a contiguous instrument range, about 64 MB. Re-encoding a block writes a new copy of its segment; every other segment is shared with the previous version by hash. A version therefore costs the segments it changed, and the hot memory of two live versions is their union.

**Proposition 7.1 [N] (versions are immutable and cheap).** A published version's bytes never change, and the memory added by publishing a version is the size of the segments it does not share with versions already resident. *Proof.* Segments are content-addressed and never rewritten (S3, S5); a manifest names hashes; two manifests naming one hash map the same file. ∎

### 7.2 Build cost

Encoding costs 10 to 11 ns per value (ST-2.4), dominated by the GCD. A full build of Norgate and Databento (about 3.3B values) takes about 35 s on one thread, and is parallel by instrument range; the ladder, summaries, presence and wavelets roughly double it. A full rebuild on the reference machine is therefore under a minute; the GCD can be made cheaper by first testing the previous block's quantum, which nearly always divides. A daily Norgate update (about 20,000 rows of five columns) is a few milliseconds of appends.

### 7.3 Where staging runs

S0 to S5 run inside the data layer's process on a staging thread pool; staging batches live on local NVMe under `staging/` and are deleted after S5. Staging never blocks readers: queries and streams read the published version throughout. At most about 1 GB of staging memory is in use at a time (one instrument range's batches and the blocks being built).

---

## 8. Appends

### 8.1 What an append is

An append is a row of an instrument whose time is after the instrument's last stored row at that level's base calendar. Every other change is a revision (S2), handled by copy-on-write and a new version. Live minute bars and the daily Norgate update are appends; vendor corrections of history are revisions.

### 8.2 Visibility by sequence number

Every append carries a sequence number (D-3.3). Per instrument and level, the append log records (sequence, visible row count) whenever the count changes, and the receipt time of each appended row. A reader with `as_of_seq = N` sees, for each instrument, the rows up to the visible count at the last entry with sequence ≤ N.

**Proposition 8.1 [N] (snapshot exactness under appends).** A reader at sequence N sees exactly the rows with sequence ≤ N, and no later append changes what it has read. *Proof.* Appends to an instrument are in time order and each extends its rows by a suffix, so the rows visible at N are a prefix of the instrument's rows, of the length the log records at N. Stored rows are never modified in place by an append: an append writes beyond the visible prefix of every reader at an earlier sequence, and the structures maintained in place are not consulted by such readers (8.3). A tail re-encoded for a late row is a new tail, the old one kept for its readers. Revisions do not modify stored rows either; they create a version (S3, S5). ∎ A binding (D-7.5) names a version and a sequence, and Proposition A-4.2 holds.

### 8.3 The tail block

Each instrument's last block per column is its tail: its values are packed horizontally (value `j` at bits `[j·W, (j+1)·W)`) with the block's base, quantum and width. An append writes the new offset in place when it fits: the value is on the block's grid (`(i − β) mod g = 0` at the block's exponent) and `0 ≤ (i − β)/g < 2^W`. Otherwise the tail is re-encoded with the new value (at most 1,023 values, about 10 µs) into a new tail, and the directory's tail pointer is swapped; readers at earlier sequences keep the old tail, which is freed when no reader holds it (an epoch counter per tail). When the tail reaches 1,024 values it is sealed: re-encoded in the lane-interleaved layout with base, quantum and width chosen afresh, appended to the instrument's run, and a new empty tail started.

There is one writer per instrument (the data layer's ingest thread for its shard). The writer stores the value, then publishes the new visible count and its append-log entry with release ordering; readers load the count with acquire ordering and never read past it. Everything the writer maintains in place (the tail's running summary, the open ladder buckets, presence bits beyond a reader's count) therefore reflects the latest sequence only, and a reader at an earlier sequence does not use it: it reduces the tail's visible prefix directly (at most 1,023 values, under 0.5 µs), assembles an open bucket from the sealed buckets of the next finer level inside it and, recursively, that level's open bucket down to the base level's visible rows, and treats a set presence bit whose row is at or beyond its visible count as absent.

### 8.4 Structures on append

| Structure | On each append | On seal |
| --- | --- | --- |
| Tail block summary | updated (n, min, max, first/last, the moments); serves readers at the latest sequence | becomes a block summary |
| Cumulative summary | — (a window reaching the tail adds the tail's visible prefix, reduced directly) | extended by one entry |
| Sparse table | — | the instrument's table extended (O(log m)) |
| Ladder | the open bucket of every level updated: max, min, last, sums (O(levels)); serves readers at the latest sequence | the bucket is sealed when its calendar's barrier passes (D-5.2) and its row appended to the level, itself an append with its own sequence |
| Presence | the label's bit set; the rank directory's counters updated at 64- and 512-bit boundaries | — |
| Wavelet matrix | not updated; queries reaching the tail scan the tail's visible prefix | the instrument's matrix for that level rebuilt (O(n log σ) for that instrument only) |

### 8.5 The live path (Databento)

The feed decoder writes each minute bar to the log (D-5.4), then appends it to its instrument's tails: about 50,000 appends a minute for 10,000 instruments and five columns, a few milliseconds at about 100 ns an append. The minute barrier then seals the minute in every open bucket of the ladder that ends with it. A late row (a minute whose bar arrives after its barrier) is an append when it is still the instrument's latest row; when it falls inside the tail's span it is inserted by re-encoding the tail under a new sequence, and readers at earlier sequences keep their view (8.3); when it falls inside a sealed block it is a revision (S2 to S5). Ladder buckets it touches that were already sealed are revised the same way. Its effect on the kernel is the late-data rule (K-4.4).

### 8.6 The daily Norgate path

When NDLake's watermark advances, S0 lands the changed year file, S1 normalises it, and S2 compares it with the store: the new day's rows are appends; any change to an earlier row is a revision. Appends go through 8.3 and are visible immediately at their sequence; revisions are built, verified and published as a new version (S3 to S5).

### 8.7 No compaction

Appends need no compaction: blocks are sealed as they fill and are final. Revisions leave superseded segments behind, collected by S6 when no binding pins them.

---

## 9. Memory budget

Norgate and Databento, all history, all hot.

| Part | Size |
| --- | --- |
| Norgate OHLC (predicted) and volume, about 148M rows | 1.3 GB |
| Databento OHLC (predicted) and volume, about 515M rows | 2.0 GB |
| Presence bitmaps and rank directories, all levels | 0.3 GB |
| Calendar ladder: Norgate week, month, quarter, year | 0.35 GB |
| Calendar ladder: Databento 5, 15, 30 minutes and 1 hour | 0.65 to 1.0 GB |
| Calendar ladder: Databento session, day, week, month | 0.03 GB |
| Block summaries, cumulative summaries, sparse tables, all levels | 0.55 GB |
| Directory and block headers (32 B for each of about 4.2M blocks) | 0.14 GB |
| Catalog structures and standing event aggregates | 0.1 GB |
| Wavelet matrices, close and volume (ST-5.7) | 0.75 GB |
| Append logs and live tails | 0.5 GB |
| **Data layer, hot** | **≈ 6.7 to 7.0 GB** |
| Staging, while it runs | ≤ 1 GB |
| Kernels (one MW14-class program < 1 GB; a study of four kernels 2 to 4 GB) | ≤ 4 GB |
| Run-scoped on-demand structures (rolling results, a non-standard calendar, a wavelet built on demand) | ≤ 2 GB |
| **Peak** (staging during a four-kernel study) | **≤ 14 GB**, inside 16 GB with 2 GB for the operating system |
| **Typical** (one kernel, no staging) | **≈ 8.5 GB** |

The figures scale measured bytes per value by estimated row counts (±25 %). The intraday ladder's range is the one real unknown: a contract that trades every minute has a fifth as many 5-minute bars as minute bars, but a calendar spread that trades a few times an hour has nearly as many, and spreads are 76 % of CL's rows. M-2 measures it on the full history; if the store exceeds 7.0 GB, the 5-minute level is the first to move from precomputed to on-demand (it is rebuilt from minutes at the encode rate, about 10 ns a value).

---

## 10. Interfaces

### 10.1 `abt-codec`

A fourth shared crate beside `abt-wire` and `abt-agg` (P-1): the block format of ST-4 (encode, decode, single-value extraction, the tail), prediction, the summaries of ST-5.1 and ST-5.2, rank and select, wavelet matrices. Pure functions over byte slices, no I/O, no semantics; property-tested for Proposition 3.2 on random and real blocks. The data layer uses it to build and query; the kernel uses it to read mapped segments (10.2); `abt-agg`'s reductions run on its decoded blocks.

### 10.2 Mapping

Published segments are immutable files on local NVMe, so the data layer can hand a reader the file instead of its bytes. The data service gains one request:

| Request | Fields | Response |
| --- | --- | --- |
| `map` | `View`, relation, columns, level, instrument range | a table of segment descriptors: path, offset, length, codec version, the directory and header arrays' locations, and the tail descriptors with their visible counts at the view's sequence |

The reader maps the segments read-only and decodes with `abt-codec`. Tails and append logs are read through `query` and `subscribe` as before, since they change. A reader on another machine uses `query` and `subscribe` only. The data layer remains the single writer of every file (A-D20).

### 10.3 Determinism

Decoding is exact (Prop. 3.2), sums are exact (Prop. 3.3), windows are exact (Prop. 5.1), ladder bars are exact (Prop. 5.2), and visibility is by sequence (Prop. 8.1), so Propositions A-4.2 (reproduction) and P-5.1 (transport invisibility) hold for mapped reads as for streamed ones.

---

## 11. ORATS

ORATS is not held hot. Measured on three consecutive days of 2025 and scaled to about 2.06B rows: its market data (contract structure, quotes, volume and open interest) needs about 9 GB at the smallest lossless encoding, and its vendor model columns (implied volatilities, smoothed volatility, theoretical values and greeks) about 78 GB at the smallest lossless encoding and about 100 GB in a fast one. The model columns do not follow Black-Scholes on `smoothSmvVol` (median relative errors 1 to 4 %), so they cannot be recomputed. Holding ORATS is the subject of a separate decision; the demand-driven design (bound chains loaded hot before a run, full history on NVMe in pcodec chain-day blocks) is the recommendation on record.

---

## 12. Amendments to the other documents

| Document | Change |
| --- | --- |
| `00-architecture.md` | A-D35 to A-D40 below; target "storage per stored value" becomes "≤ 2 B per value resident on average with every precomputed structure, about 6.7 to 7.0 GB for Norgate and Databento" |
| `01-data.md` | D-2.1: prices and quantities are tick columns (ST-3), `f64` is a view; D-3.1 and D-3.2: the resident form is ST-4, partitions are ST-7 segments; the log, versions, snapshot reads, the cache and the tiers stand |
| `04-protocol.md` | `abt-codec` beside `abt-wire` and `abt-agg`; the `map` request in the data service |
| `06-performance.md` | X-2.1's store structures are superseded by ST-4 and ST-5 for Norgate and Databento |
| `05-migration.md` | M-2 builds `abt-codec`, the staging pipeline and the hot store; the encoding studies are its first benchmarks |

| # | Decision | Rejected alternative, and why |
| --- | --- | --- |
| A-D35 | Prices and quantities are integer ticks with a per-block quantum (exponent × GCD); floats are views | `f64` (or `f32`) storage: sums are not associative, Databento's ticks are not `f32`, and the GCD saves 5 to 19 bits a value on binary-fraction ticks (ES's 0.25 to ZT's 1/256) |
| A-D36 | The resident form is instrument-contiguous, lane-interleaved blocks with OHLC prediction and patched exceptions, memory-mapped | uncompressed columns (about 26 GB and slower); entropy coding in memory (ten to twenty times slower to decode) |
| A-D37 | Everything program-independent is precomputed and hot (ST-5); program-dependent results are recomputed in run-scoped memory | caching rolling results: unbounded and incompressible |
| A-D38 | A seven-stage staging pipeline with verification by decoding, atomic publication and content-addressed segments shared between versions | rebuilding the store per update: minutes instead of milliseconds, and no structural sharing |
| A-D39 | Appends in place into tail blocks, visible by sequence number; revisions as copy-on-write versions | appends as new versions: a version per minute live |
| A-D40 | ORATS is not hot; its holding is decided separately | any hot ORATS design: at least about 87 GB losslessly |

---

## 13. Tests and gates

| Test | Statement |
| --- | --- |
| `abt-codec` property tests | encode then decode is the identity on random blocks at every width 0 to 64, with and without exceptions, with `recip_exact` set and unset; single-value extraction equals the decoded block |
| Real-data round trip | every block of the staged Norgate and Databento store decodes bit for bit to the vendor's values (S4 over the whole store, once, at M-2) |
| Exact aggregates | sums, moments and windows from cumulative summaries equal a 128-bit rational reference on random windows |
| Ladder | every precomputed bar of a sample of instruments equals the bar recomputed from base rows |
| Appends | interleaved appends and reads at random sequences: every reader sees exactly the rows with sequence ≤ its own (Prop. 8.1); tails re-encoded under readers keep the readers' views |
| Revisions | a revision publishes a version whose unchanged segments are shared by hash, and readers of the old version are unaffected |
| Performance | `scripts/study/encoding/` benchmarks rerun on the reference machine at M-2 and M-8: decode-and-sum at or below the raw `u32` scan at every width up to 27; the hot store within the budget of ST-9 |
| Parity | G1 unchanged: MW14 and R8L read their bars through the new store in `two_pass` mode and reproduce their references |

---

## 14. What is established, new and decided in this document

- [E] Frame-of-reference and patched frame-of-reference coding (Zukowski, Héman, Nes and Boncz 2006); the FastLanes layout (Afroozeh and Boncz 2023); ALP's verified decimal reconstruction (Afroozeh, Kuffó and Boncz 2023); rank and select directories (Jacobson 1989; Vigna 2008); wavelet matrices and range quantiles (Claude, Navarro and Ordóñez 2015; Gagie, Puglisi and Turpin 2009); roaring bitmaps (Lemire et al. 2016); sparse tables for range minima (Bender and Farach-Colton 2000); pcodec (Loncaric 2024).
- [N] Lossless representation (3.2); exact sums across blocks of different quanta (3.3); exact windows from cumulative summaries (5.1); exact ladder (5.2); immutable versions with structural sharing (7.1); snapshot exactness under appends (8.1).
- [D] A-D35 to A-D40.
