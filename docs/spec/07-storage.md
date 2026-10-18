# 07 · Storage: the hot store for Norgate and Databento

Oct 15, 2026 · the resident format, the precomputed structures, the staging pipeline and the append path of the data layer, decided from measurements on the company's own data. Tags and cross-references as in `00-architecture.md`; sections of this document are cited as `ST-n`, and the stages of the staging pipeline as S0 to S6 (ST-7).

This document supersedes, for Norgate and Databento: the `f64` representation of prices and quantities (D-2.1, the [D] after the type table); the resident layout of D-3.1 and D-3.2 (time-major Arrow and Parquet partitions as the in-memory form); and the store structures of X-2.1. Everything else in `01-data.md` stands: the log as the source of truth, versions and bindings, snapshot reads by sequence number, the query forms, the streams and clock barriers, the cache, the resolver. ORATS is out of scope here (ST-11).

---

## 1. The result in one paragraph

Every Norgate and Databento value is held hot and locked in memory, about 3.3 billion values in roughly 6.8 to 7.2 GB with every precomputed structure, inside the 16 GB machine with room for the kernel and the query layer. Prices and quantities are exact integer ticks: each block of 1,024 values of one instrument carries a quantum found from the data (a decimal exponent times a greatest common divisor), so every vendor value converts to an integer and back bit for bit. Blocks are stored instrument-contiguous and lane-interleaved (the FastLanes layout), offsets from a base, with OHLC predicted from the close and rare outliers patched, and a decode-and-sum runs at 0.18 to 0.38 ns a value, faster than scanning the same values uncompressed (0.63 to 0.85 ns as `i64`), because the scan is bandwidth-bound and the packed form is two to six times smaller. Everything that does not depend on a program is precomputed and hot: cumulative block summaries that answer any window, block min and max with sparse tables, the full calendar ladder of bars, presence bitmaps with rank directories, the catalog structures with interval indexes, standing event aggregates, a revision table for receipt-time reads, and wavelet matrices for order statistics. Every encoding and every structure answers a range of rows without decoding outside the blocks the range covers (ST-6), so a range max, a quantile or an OHLCV bar over 30,000 rows costs 0.6 to 4 µs instead of the 20 to 60 µs of a scan. Data enters through a seven-stage staging pipeline that verifies every block by decoding it, publishes versions atomically and shares unchanged blocks between versions; appends are written in place into each instrument's tail block and become visible by sequence number, so a reader bound to an earlier sequence never sees them, and revisions of history become new versions by copy-on-write. Durability comes from the log and a seal journal, so a restart rebuilds the tails from the log and serves again within 30 s; a weekly consolidation, the store's only compaction, copies sealed blocks back into instrument order without changing a value. Adjusted series (split, total return, continuous futures) are views evaluated piecewise between events, prices cross the protocol as exact decimals, and the futures calendars are fixed by trade date with UTC-aligned intraday buckets.

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

| Range query on rows `[a, b)` of one column, width 11, 33.5M values, random positions (`lanes-bench range`) | Packed, with the precomputed structures | Uncompressed `u32`, scanned |
| --- | --- | --- |
| point (one row) | 26 ns | 12 ns |
| series read + sum, 1 to 32 rows (mean 16); the same queries as a batch sorted by start | 206 ns; 87 ns | 42 ns; 35 ns |
| series read + sum, 32 to 1,024 rows (mean 527) | 686 ns; 345 ns | 384 ns; 186 ns |
| series read + sum, 1,024 to 65,536 rows (mean 33,321) | 13.4 µs; 10.7 µs | 15.3 µs; 5.3 µs |
| range max (sparse table + edges), the same three lengths | 220 ns / 733 ns / 1.34 µs | 87 ns / 519 ns / 19.4 µs |
| OHLCV bar over an arbitrary range (two points, a max, a min, a sum), the same three lengths | 677 ns / 2.25 µs / 3.93 µs | 197 ns / 2.03 µs / 59.5 µs |
| median of a range (wavelet matrix), 20 to 300 / 300 to 3,000 / 3,000 to 30,000 rows | 908 / 829 / 582 ns | copy and select: 651 ns / 3.50 µs / 31.1 µs |
| count of values below `c` in a range (wavelet matrix), any length | 306 to 349 ns | a scan of the range |

Three findings shape ST-6. A block a range covers by a quarter or more must be decoded with the fully unrolled kernel (shifts are constants); the row-range kernel, whose shifts are computed at run time, decoded long ranges at 28.8 µs against 13.4 µs, so it is used for short edges only. Short random ranges pay one more dependent cache miss than an uncompressed column (the block's header) and the decode instructions, which shrink the overlap between independent queries: 3 to 5 times an uncompressed read, 0.1 to 0.2 µs in absolute terms; sorting a batch halves it. A range read many times while it fits in cache is faster from a decoded copy (5.3 against 10.7 µs above), which is what the run-scoped decoded-block cache is for.

### 2.5 Rejected alternatives

| Alternative | Why not |
| --- | --- |
| Uncompressed resident columns (`i64` ticks and `f64` values, two layouts) | about 26 GB for the values alone, and slower than the packed form on every scan (2.4) |
| pcodec (distribution-aware entropy coding) in memory | smallest (about 2.5 GB in total; 1.24 B/value for NDLake 2024 close, 0.32 for ZN), but decodes at 2.5 to 5.5 ns/value, ten to twenty times slower than the lane-interleaved form; kept for the cold tier only (S6 in ST-7) |
| Deltas instead of offsets | about 10 % smaller, but decoding needs a sequential running sum (1.23 against 0.99 ns/value fused); offsets keep every lane independent |
| Rebuilding vendor greeks or prices from a model | not applicable to Norgate and Databento; for ORATS the model columns do not follow Black-Scholes (ST-11) |

### 2.6 What these measurements do not cover

| Not covered | Why it matters | Measured by |
| --- | --- | --- |
| The reference machine (Apple M4: NEON, 16 KB pages, 10 cores, unified memory) | every figure above comes from a 2-core x86 VM; vectorisation and memory latency differ | G-ST1 (ST-13) |
| A 7 GB working set | random access across 7 GB adds TLB misses the sets above (at most about 200 MB) did not have | G-ST1 |
| Blocks of mixed widths | the range and lane benches use one compile-time width per run, so the per-block dispatch (a table of 65 decoders indexed by width) is unmeasured | G-ST1 |
| Reconstruction in the hot loop | OHLC from residuals, exponent alignment, exception patching and the `f64` view are each measured alone or not at all | G-ST1 |
| Parallel scans | 10 threads against the M4's memory bandwidth | G-ST1 |
| Sizes of the full history | ST-9 scales two days of six roots and two Norgate years (±25 %); the intraday ladder is the largest unknown | the sizing run, G-ST2 (ST-7.4) |

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

A block of 1,024 offsets is 32 lanes of 32 values: value `j` lives in lane `j mod 32` at row `j div 32`, and row `i` of every lane occupies bits `[i·W, (i+1)·W)` of that lane's `W` words, so the packed block is `32·W` 32-bit words (64-bit words for `W > 32`). Unpacking row `i` is the same shift and mask on 32 adjacent words, which compilers vectorise (8 lanes per AVX2 instruction, 16 with AVX-512, 4 with NEON on the M4). A single value is extracted with one or two word reads. Rows `[a, b)` of a block are rows `a div 32` to `⌈b/32⌉` of every lane, with the lanes outside `[a, b)` masked in the first and last row: offsets are random access, so no row before `a` is decoded. Decoding uses the fully unrolled kernel (one per width, shifts as constants) when the range covers at least 256 values of the block, and the row-range kernel otherwise (ST-2.4). Reference: Afroozeh and Boncz, *The FastLanes Compression Layout*, VLDB 2023.

### 4.3 Header

32 bytes per block: `base: i64`, `g: u64`, `e: i8`, `W: u8`, `n: u16`, `flags: u16` (`recip_exact`, `patched`, `predicted`), `exceptions: u32` (offset into the block's exception list, 0 when none), padding to 32. Headers are stored apart from the packed words (a header array per column, an instrument's headers contiguous), so a scan reads headers sequentially and a range reads the headers of its blocks from adjacent cache lines.

### 4.4 Patched exceptions

Per block the width is `W* = argmin_W [ n·W + 74·#{u > 2^W − 1} ]`, 74 bits being the cost of an exception (its 10-bit position and 64-bit value). Values above the width are stored in the block's exception list, sorted by position, and written back after unpacking; a range applies only the exceptions whose positions fall in it, found by binary search. Volume blocks use this routinely (a rebalance day or a 10× spike widens one block otherwise); price blocks rarely need it.

### 4.5 OHLC prediction

A bar relation with open, high, low and close stores four tick columns per instrument: `C`; `H′ = H − max(O, C)`; `L′ = min(O, C) − L`; `O′ = O − C_prev`, with `C_prev` the previous row's close of the same instrument and, for the instrument's first row, `O′ = 0` and `O` stored in the instrument's directory entry (ST-4.8). The residuals are computed on integers at the finest exponent of the four columns over the block's rows, so they are exact; each residual column then has its own block headers, and its quantum is found from its own values (the GCD of `H′` is usually the close's tick). Decoding is `C`, then `O = O′ + C_prev`, then `H = H′ + max(O, C)` and `L = min(O, C) − L′`, element-wise on decoded blocks after aligning exponents (an integer multiplication by `10^{e* − e}`, exact). `H′ ≥ 0` and `L′ ≥ 0` hold on vendor data; a row violating them (a vendor error) is stored as an exception with its raw value and reported by S1.

The prediction is row-local except for `C_prev`, so it never forces decoding outside a range: rows `[a, b)` need the close at row `a − 1` only, which is one extraction from the same block or the previous block's `last` (its block summary). Every summary, zone map, sparse table and wavelet matrix is built over the reconstructed values `O`, `H`, `L`, never over the residuals, which have no meaning to a predicate or an aggregate.

### 4.6 Other columns

Volume and open interest are tick columns with `e = 0`. Sparse columns (Norgate `div_cash`, `split_ratio`; corporate actions) are not dense columns but per-instrument event lists (ST-5.5). Constant columns per instrument (a security's exchange, a contract's multiplier) live in the registry.

### 4.7 Time

No per-row timestamp is stored. Each instrument has, per calendar level, a presence bitmap over the level's labels (ST-5.4), and the `k`-th set bit is row `k`. Receipt time follows the source's convention, recorded per version: Norgate bars are available at the bar's close plus the version's processing delay; Databento historical `ohlcv-1m` carries no `ts_recv` and stamps each bar at its open, so receipt is the bar's close (`ts_event` + 60 s) plus the configured delay (B-3, D-7.7); a bar is never available before the minute it summarises has ended. Rows appended live carry their own receipt time in the append log (ST-8.2), not in blocks.

### 4.8 Directory and block tables

Per (relation, column, level, instrument), the directory holds the block count, the first and last label indices, the tail's location, for bar relations the first row's open (ST-4.5), and the instrument's **block table**: one 40-byte entry per sealed block, its header (ST-4.3) and its locator (segment id `u32`, word offset `u32`). The table is a chunked array of 256-entry chunks, so it grows by appends without moving, and the block of row `r` is `table[r div 1024]`, O(1). A sealed block lives in a base segment (instrument-contiguous, written by S3 or by consolidation, ST-8.7) or in an append segment (ST-8.3); the table hides which. The header arrays of ST-4.3 are the tables' header fields.

---

## 5. Precomputed structures

Everything that is a function of the stored data alone, independent of any program, is precomputed at staging and kept hot. What depends on a program's parameters is answered from these structures or recomputed in run-scoped memory (ST-5.8).

| Structure | Granularity | Answers | Size (both sources, all levels) |
| --- | --- | --- | --- |
| Block summary over the reconstructed values: `n`, min, max, the row of each, first, last | per block | zone-map pruning; block-aligned min, max, argmin, argmax, first, last | in the 0.55 GB below |
| Cumulative summary: `count`, `Σx`, `Σx²`, `Σk·x` exact in the instrument's finest exponent | per instrument at every block boundary | any range's moments, mean, variance and trend slope: whole blocks from two cumulative entries, the edges decoded | 0.55 GB together with the block summaries and sparse tables |
| Cumulative cross-moments of bar relations: `Σ C·V` and `Σ (H+L+C)·V`, exact | per instrument at every block boundary | dollar volume and VWAP (close or typical price) over any range | 0.03 GB |
| Sparse table over block min and max, with the block of each | per instrument | range min, max, argmin, argmax over whole blocks in O(1) | (above) |
| Calendar ladder (ST-5.3) | per instrument per level | any bucket of a standard calendar in O(1); coarser levels from finer | 1.0 GB |
| Presence bitmap + rank directory per level (ST-5.4) | per instrument | (instrument, label) → row in O(1); a label range → a row range; the number of rows in a label range | 0.3 GB |
| Catalog structures (ST-5.5) | per instrument, per index, per root | identity, symbols, actions, membership at a date and over a range, instruments alive at a label or over a range, delistings, classification, calendars, contract specs, roll map | 0.1 GB |
| Standing event aggregates (ST-5.6) | per instrument | adjustment factors as step functions | (in the catalog) |
| Wavelet matrices (ST-5.7) | per instrument, close and volume, per level | range median, quantile, k-th, count and listing of the values in `[lo, hi)`, in O(log σ) | 0.75 GB |
| Revision table (ST-5.9) | per version | the values a revision superseded, with their receipt intervals: receipt-filtered reads and ranges | 0.01 GB |
| Event-interval summaries (ST-5.10) | per instrument per split, dividend or roll | raw cumulative moments at each event row, and min, max and their rows between consecutive events: adjusted series over any range without decoding between its ends | 0.05 GB |

### 5.1 Windows from cumulative summaries

The sum over rows `[a, b)` of an instrument is `C(b) − C(a)`, where `C(k)` is the cumulative total at the start of `k`'s block plus the sum of the rows of that block before `k`. The in-block part decodes the shorter side only: the prefix when it is at most 512 values (unpacking only the rows it spans), otherwise the block total minus the suffix. A window of at most two blocks is decoded directly instead. Cost (ST-2.4): about 0.6 to 0.75 µs per random window whatever its length, dominated by the two cache misses, against a scan that grows with the length (4.6 µs at 2,000 to 22,000 rows). Sequential rolling windows (the kernel's and the aggregate pass's case) use sliding states over decoded blocks, 1.8 ns per output, and need no lookup at all. The same rule gives every cumulative quantity over any range: `count`, `Σx`, `Σx²` (so mean and variance), `Σk·x` (with `Σx` and `Σk`, the least-squares slope against row index, re-based to the range by `Σ(k−a)·x = Σk·x − a·Σx`), and the cross-moments `Σ C·V` and `Σ (H+L+C)·V` (dollar volume, VWAP). A reader uses a block's summary only if its visible count (ST-8.2) covers the whole block; otherwise it reduces its visible rows of that block directly.

**Proposition 5.1 [N] (exact windows).** Window moments computed from cumulative summaries equal those of a direct reduction of the window, exactly. *Proof.* Proposition 3.3: every term is an exact integer in the instrument's finest exponent, and subtraction of exact integers is exact. ∎ Order statistics have no cumulative form; they use ST-5.7.

### 5.2 Min and max over ranges

A range's minimum is the minimum of the sparse-table answer over its whole blocks and the two edge minima, the edges decoded as in ST-5.1; the sparse table keeps the block of each extremum, so the row of the minimum (the time of a high or a low) is that block's argmin from its summary, or the edge's. Measured: 1.34 µs for ranges of 1,024 to 65,536 rows, against 19.4 µs for a scan (ST-2.4). The sparse table has `⌈log₂ m⌉` rows over an instrument's `m` blocks; instruments have few blocks per level, so it is small.

### 5.3 The calendar ladder

| Source | Base | Levels precomputed |
| --- | --- | --- |
| Norgate | day | week, month, quarter, year |
| Databento | 1 minute | 5, 15 and 30 minutes, 1 hour, session (per root, from the session table), day, week, month |

Each level is a bar relation (open, high, low, close, volume, and the number of base rows in the bucket) in the block format of ST-4 with its own summaries, presence and wavelets. A level is built from the finest level it coarsens (week and month from day; quarter from month; 15 minutes from 5), never from the base when a finer built level exists.

**Proposition 5.2 [N] (exact ladder).** Every precomputed bar equals the bar computed from the base rows of its bucket. *Proof.* Open, high, low and close are first, max, min and last, which are exactly associative (they select one input value); volume and the row count are sums of integers, exact by Proposition 3.3; a coarser bucket is a union of finer buckets because the ladder's calendars are ordered by refinement (V-3.3, Prop. 3.3), so building from a finer level is a regrouping of the same inputs. ∎ Calendars outside the ladder are built on demand from the nearest finer level (ST-5.8).

**Calendar rules.** Every bucket of the ladder is defined below; a different rule would give different bars without any error, so these are part of the format and change only with a new version.

| Level | Bucket | Label | Receipt |
| --- | --- | --- | --- |
| Databento 1 minute (base) | Databento's bar `[ts_event, ts_event + 60 s)`, `ts_event` the bar's open, UTC | `ts_event` | the bar's close plus the delay (ST-4.7) |
| 5, 15, 30 minutes, 1 hour | UTC-aligned: `[⌊t / P⌋·P, ⌊t / P⌋·P + P)` | the bucket's start | the bucket's end plus the delay |
| session | the root's session from the session table in force on that date, `[open, close)` converted from exchange local time to UTC for that date, early closes included; a change of the table is a new version of the table | the session's trade date | the session's close plus the delay |
| day (futures) | every session with the same trade date; on CME Globex one session (17:00 CT the evening before to 16:00 CT), so the level aliases the session level and is stored once | trade date | as session |
| week | the trade dates of one ISO week (Monday to Sunday) | the bucket's last trade date | the last session's close plus the delay |
| month, quarter, year | the trade dates of the calendar month, quarter, year | the bucket's last trade date | as week |
| Norgate day (base) | the vendor's trading date | date | the bar's close plus the version's delay (ST-4.7) |
| Norgate week, month, quarter, year | as for futures, on trading dates | the bucket's last trading date | as week |

CME (America/Chicago) and ICE US (America/New_York) sit at whole-hour offsets from UTC in both seasons, so UTC-aligned buckets of 5 minutes to 1 hour coincide with exchange-local ones and no bucket is split or merged at a daylight-saving change; sessions are computed in exchange local time per date, so a session keeps its local hours across the change. Days are trade dates, never UTC dates, which would cut every Globex session in two. A bucket with no base rows has no row (Databento emits no bar for a minute without trades); every bar carries the number of base bars it summarises. A bucket's receipt is its end even when its last base bar came earlier, because the bucket is complete only when it ends.

**Bars over any range.** A range aligned to a level (the months of 2015 to 2020) is a row range of that level. A bar over an arbitrary row range `[a, b)` of any level is computed from that level's structures without the ladder: open is the row `a` value, close the row `b − 1` value, high a range max of `H`, low a range min of `L`, volume a range sum of `V` (ST-5.1, ST-5.2). Measured: 3.9 µs over 1,024 to 65,536 rows, against 59.5 µs for scans (ST-2.4). The ladder is what makes a coarse calendar's bars a contiguous series for a sequential pass; ranges do not depend on it.

### 5.4 Presence and rank

Per instrument and level, a bitmap over the level's labels within the instrument's alive interval marks the labels at which it has a row. A rank directory (a cumulative popcount every 512 bits plus a 9-bit relative count every 64 bits, about 25 % overhead; Jacobson 1989; Vigna 2008, rank9) answers "the row of label `ℓ`" in O(1), and select answers "the label of row `k`". The minute presence of futures is the larger part (about 0.23 GB before the directory); equity day presence is about 20 MB.

### 5.5 Catalog structures

| Relation | Structure |
| --- | --- |
| Security master, vendor ids, tickers | interval indexes keyed by (vendor, key) and by symbol, binary search on time (D-2.4) |
| Corporate actions (splits, dividends, delistings) | per-instrument event lists in time order; the events in a range by binary search |
| Index membership (52 indexes) | the vendor's intervals (191,143 rows) in an augmented interval tree per index (members at any time in `[t0, t1]` in O(log n + k)), plus, per index, a compressed bitmap of member instruments per day (members at `t` in O(1); roaring bitmaps, Lemire et al. 2016), a few MB |
| Instruments alive | per level, an augmented interval tree over every instrument's alive interval: the instruments alive at a label or during a range, in O(log n + k), what `section` and panel queries enumerate |
| Classification, fundamentals | per-instrument event lists |
| Trading calendars, futures sessions | per calendar a sorted label array and the session table |
| Futures definitions | per contract: tick, multiplier, expiry, activation, unit; spreads inherit their legs' tick and are verified by the block GCD |
| Continuous roll map | per root, the contract held at each date with its additive adjustment, as an interval list; the contracts held over a range by binary search |

### 5.6 Standing event aggregates

Some program-independent results are needed by every equity program and are pure arithmetic over catalog events: the cumulative split factor and the cumulative total-return factor per instrument. They are declared by the kernel's `catalog` library as standing aggregate requests (cumulative product of a column of an event relation per entity, D-6.1), which the data layer evaluates at staging and stores as step functions: one entry per event. An adjusted price or a total-return index value is then the stored tick value times the step function's current factor, one multiply on read; the adjustment between two dates is the ratio of the factors at the two ends, two binary searches. Split factors are stored as exact rationals; the total-return factor is an `f64` product taken in event order, so it is deterministic. The semantics stay in the request, which the kernel authors; the data layer computes a product (A-D6).

### 5.7 Order statistics

Range median, quantile and k-th smallest have no cumulative form. They are answered by a wavelet matrix per (instrument, column, level) over the rank-reduced offsets, in O(log σ), σ the number of distinct values of the instrument's column (Claude, Navarro and Ordóñez 2015; Gagie, Puglisi and Turpin 2009 for range quantiles). A matrix costs about `⌈log₂ σ⌉ · 1.25` bits a value (14 to 17 bits for a price or a volume over an instrument's history) plus the sorted distinct values. Built at staging for close and volume at Norgate's day level and coarser and at Databento's 1-hour level and coarser (about 0.75 GB); open, high and low, and finer futures levels, are built on first demand (about 0.35 GB a column at Norgate's day level) and kept for the life of the version. Doing all five columns at staging would cost about 1.8 GB; the extra 1 GB stays as headroom in ST-9.

On a row range `[a, b)` a matrix answers: the k-th smallest (so median and any quantile), the number of values in `[lo, hi)` (so the rank of a value, the fraction above a threshold), and the listing of those values with their rows, each in O(log σ) plus the output. Measured: a median in 0.6 to 0.9 µs and a count in about 0.3 µs whatever the range's length, against 31 µs to copy and select over 3,000 to 30,000 rows (ST-2.4). A matrix covers sealed blocks only; a range that reaches the tail adds the tail's visible values by counting: a count adds a count over the tail, and a k-th smallest is found by binary search on the value with a count on each side, O(log σ) counts.

### 5.8 What is not precomputed, and why

| Not precomputed | Because | Answered by |
| --- | --- | --- |
| A program's rolling results (a 252-row mean, a 60-day ADV) | they depend on the program's parameters, there are unboundedly many, and each is a float column that does not pack (8 B per row) | the cumulative summaries (random access) or a sliding state in the aggregate pass (sequential, 1.8 ns per output), in run-scoped memory |
| Running totals per row | they would double or triple the store for a saving of one or two cache misses per window | the cumulative summaries at block boundaries |
| Calendars outside the ladder | unbounded | built on demand from the nearest finer level and cached until the version is evicted |
| Wavelet matrices below 1 hour for futures, and for open, high and low | about 1 GB per column at 5 minutes; order statistics of raw open, high and low are rarely asked | built on first demand |
| Order statistics of derived series (returns, dollar volume, a ratio) | the series is a program's expression | a sliding order-statistic structure in the aggregate pass (O(log w) per output), or a wavelet matrix built in run scope over the derived column |
| Range sums of derived float series (realised variance of returns, a sum of log ratios) | returns are ratios, with no exact integer form, and which return (adjusted or not, close to close or open to close) is the program's choice | in run scope, one pass (1 to 2 ns a value) builds per-block partial sums of the derived column in canonical order; a range is then the sum of its whole blocks' partials in block order plus the decoded edges, O(range / 1,024), deterministic |
| Moments across two instruments (a covariance of two series over a range) | quadratic in the number of instruments | the aggregate pass over the two series |
| Decoded copies of blocks | the packed form is the resident form | a run-scoped decoded-block cache (LRU, default 256 MB) for ranges read repeatedly (ST-2.4) |

### 5.9 Revisions and receipt-time reads

D-2.2 keeps every revision of a fact and answers a receipt filter `r ≤ r_max` with the latest revision it admits. Blocks hold the latest revision of each row; the revision table holds what revisions superseded. Each entry is (relation, column, instrument, row, the superseded value's bits, its receipt interval `[r_from, r_to)`), and a row inserted by a revision has an entry whose value is *absent* before its receipt. The table is sorted by (relation, column, instrument, row) and indexed by `r_to`; it is built by S3 and carried, copy-on-write, by every later version.

A read with a receipt filter decodes its range as usual, then substitutes, for each entry of the range (binary search, O(log R + entries)) whose interval contains `r_max`, the superseded value. Rows whose receipt follows `r_max` are excluded: for historical rows receipt is event time plus the version's delay, so they are a suffix of the range; for appended rows it is the receipt recorded in the append log (ST-8.2). Aggregates over a range with active entries are corrected exactly for sums and counts (subtract the latest value, add the superseded one: integers) and for counts in the wavelet matrix (±1 per entry); min, max and order statistics decode the blocks that hold active entries and combine them with the structures' answers over the other blocks. Revisions are vendor corrections, rare, so the table is small (ST-9). The stream emits, at each row's first receipt, the value then known (the oldest entry's value when the row has entries), and each later revision at its own receipt, from the table's `r_to` index (D-5.3).

### 5.10 Adjusted series over ranges

Adjusted series are views over the stored raw values: `x_adj(k) = x(k) · F(k)` for multiplicative adjustments (the split factor, the total-return factor, a ratio-adjusted continuous future) and `x_adj(k) = x(k) + A(k)` for additive ones (a back-adjusted continuous future), with `F` and `A` the step functions of ST-5.6 or the roll map of ST-5.5, constant between events. Which adjustment a read uses is named in its request (the kernel's choice, A-D6); the data layer evaluates it. No adjusted copy is stored: there would be one per adjustment, and every split or dividend would rewrite an instrument's history.

A range `[a, b)` is cut at the events inside it (binary search, O(log E + p) for `p` pieces). Splits are rare (`p` is 1 to 3 over most ranges); dividends are not (a quarterly payer gives about 40 pieces over ten years of a total-return series), and a monthly-rolled future gives about 120 over ten years. So the pieces between events are precomputed as **event-interval summaries**: at each event row, the raw cumulative moments (the cumulative summary of ST-5.1 evaluated at that row), and for each interval between consecutive events, the raw min and max with their rows. For the registry's continuous series (D-2.4), the same summaries are kept per roll interval of the held contract. A piece between two events then costs two lookups and no decoding; only the range's two ends decode their edges.

| Query on an adjusted range | Evaluation | Exactness |
| --- | --- | --- |
| sum, mean, moments | `Σ_p F_p · S_p` (or `Σ_p (S_p + n_p · A_p)`), `S_p` each piece's raw sum from event cumulatives | splits and additive adjustments: exact, in 256-bit integers over the factors' common denominator (split ratios are stored as rationals); total return: the `f64` sum over pieces in time order of `F_p · f64(S_p)`, a definition both layers compute through `abt-agg`, so it is deterministic and bit-identical across layers |
| dollar volume, VWAP | the raw cross-moments of ST-5.1: a split scales price and volume inversely, so `Σ C·V` needs no adjustment for splits | exact |
| min, max, argmin, argmax | per piece, `F_p > 0` preserves order: `max_p (F_p · max_p raw)`, from the event-interval summaries and the two edges | exact for splits and additive; the defined `f64` products for total return |
| k-th, median, quantile, count in `[lo, hi)` | counts: `Σ_p count_raw(piece_p, threshold_p)` with each threshold converted to the piece's raw ticks (`x·num ≥ c·den` in integers for rational factors; the same `f64` comparison as the view for total return); k-th: binary search on the adjusted value with those counts. With more than 8 pieces, the run builds a wavelet matrix over the adjusted column for that instrument on first demand (about 10 ns a value) and answers from it | as the counts |

Continuous futures are the same algebra with pieces in different instruments: each piece is a row range of the contract held over it.

---

## 6. Range queries

### 6.1 The property

**Proposition 6.1 [N] (range closure).** For every encoding and every structure of ST-3 to ST-5, a query over the rows `[a, b)` of one instrument reads only the blocks that intersect `[a, b)` with their headers and summaries, O(1) entries of the cumulative summaries, sparse tables and rank directories, and O(log σ) entries of a wavelet matrix; it needs the value of no row outside `[a, b)` except the close at row `a − 1`. (The decoder may still decode a whole intersecting block when that is faster, ST-4.2.) *Proof.* Offsets are per-block frame-of-reference values, so every row is addressed by its block and lane without decoding earlier rows (ST-4.2); quanta, bases and widths are per block (ST-3); exceptions are sorted by position (ST-4.4); the prediction of ST-4.5 is row-local except for `C_prev`; deltas, which would need every earlier row of a block, are not used (ST-2.5). Sums, moments and cross-moments decompose as differences of cumulative summaries (ST-5.1); min and max as overlapping sparse-table cells (ST-5.2); order statistics and counts are wavelet-matrix range operations (ST-5.7); presence ranges are rank differences (ST-5.4); catalog ranges are interval-tree or binary-search queries (ST-5.5, ST-5.6); revisions are a sorted table (ST-5.9). ∎

The one encoding that does not have the property is the cold tier's (pcodec pages decode as a whole); it is never queried in place. A segment restored from the cold tier is restored whole, in the hot form, before it is read.

### 6.2 Audit

Each encoding and structure, the ranges it answers, and how.

| Encoding or structure | Ranges it answers | How | Cost |
| --- | --- | --- | --- |
| Tick values, per-block quantum (ST-3) | any row range | each block decoded at its own exponent and scaled to the range's finest, an exact integer multiplication | per block, none extra |
| Lane-interleaved offsets (ST-4.2) | any row range, any single row | rows `a div 32` to `⌈b/32⌉`, edge lanes masked; the unrolled kernel for blocks covered by ≥ 256 values | 26 ns a point; 13.4 µs for 33,000 rows, below an uncompressed scan |
| Patched exceptions (ST-4.4) | any row range | binary search in the sorted exception list | O(log e) |
| OHLC prediction (ST-4.5) | any row range | the close at `a − 1` from the block or the previous block's `last` | one extraction |
| Tail block (ST-8.3) | any row range within the visible count | horizontal packing, random access | as above |
| Block summaries (ST-5) | block-aligned ranges; pruning of any range | over reconstructed values, used only when the reader's visible count covers the block | O(1) per block |
| Cumulative summaries and cross-moments (ST-5.1) | sum, count, mean, variance, slope, dollar volume, VWAP of any range | two cumulative entries, the shorter edges decoded | 0.6 to 0.75 µs at any length |
| Sparse tables (ST-5.2) | min, max, argmin, argmax of any range | two overlapping cells plus the edges | 1.3 µs for 1,000 to 65,000 rows |
| Calendar ladder (ST-5.3) | ranges aligned to a level, as row ranges of that level; any bar over any range from the level's own structures | as the rows above | 3.9 µs for an OHLCV bar over 1,000 to 65,000 rows |
| Presence and rank (ST-5.4) | a label range to a row range; the number of rows in a label range | two ranks | O(1) |
| Wavelet matrices (ST-5.7) | k-th, median, quantile, count and listing of values in `[lo, hi)` over any row range; ranges into the tail by counting | O(log σ) rank operations | 0.6 to 0.9 µs a median, 0.3 µs a count |
| Catalog (ST-5.5) | members of an index, instruments alive, actions, tickers, roll map, contracts, over a date or a range | interval trees, sorted interval lists and event lists | O(log n + k) |
| Standing event aggregates (ST-5.6) | the adjustment between any two dates | ratio of the factors at the two ends | two binary searches |
| Revision table (ST-5.9) | receipt-filtered reads and aggregates over any range | binary search on (instrument, row); exact corrections for sums and counts | O(log R + entries) |
| Append log (ST-8.2) | rows visible at a sequence, receipt-filtered tail rows | the visible count at the reader's sequence | O(log) + the tail's rows |
| Directory and block tables (ST-4.8) | instruments in an id range; any block of an instrument wherever it is stored (base or append segment) | instruments contiguous by id; `table[r div 1024]` | O(1) per block |
| Event-interval summaries (ST-5.10) | adjusted sums, moments, extremes and quantiles over any range | pieces between events from the summaries, the two ends decoded | O(p) lookups, `p` pieces |

Ranges across instruments (a panel: the instruments of a set over a date range) are the alive-interval or membership query of ST-5.5 followed by one row range per instrument; ranges across levels are one level's row range. A batch of range queries is sorted by (instrument, start) before it runs, so blocks are visited in order (87 against 206 ns for short random ranges, ST-2.4).

### 6.3 The query forms

How each form of D-4.1 runs, with its cost.

| Form | Algorithm | Cost |
| --- | --- | --- |
| `point` (instrument, label) | rank of the label (presence), directory and header, lane extract | three dependent misses: about 0.2 to 0.3 µs alone, about 30 to 50 ns each in a batch (26 ns measured for the extract) |
| `latest` (instrument, ≤ t) | rank of t's label, select the previous set bit, extract | as `point` |
| `series` (instrument, [t0, t1]) | rank both ends, decode the covered rows (6.2) | 0.2 µs for a few rows; 0.4 ns a row over long ranges, below an uncompressed scan |
| `section` (label) | the alive-interval tree gives the instruments alive at the label; for each, the presence bit and an extract | O(alive): about 0.3 to 0.5 ms for 10,000 instruments from memory, within A-7.3's 1 ms; during a replay, cursors hold the current blocks and a section is read from them |
| panel (instruments × [t0, t1]) | the alive or membership query of ST-5.5, then `series` per instrument, sorted | the sum of the series |
| `scan` with a predicate | block summaries prune whole blocks; survivors decoded and filtered; a value predicate on a column with a wavelet matrix lists the matching rows directly | proportional to surviving blocks, or O(log σ) per match |
| `aggregate`, sequential window | decode + sliding state | 1.8 ns per output |
| `aggregate`, random window (sum, count, mean, variance, slope, dollar volume, VWAP) | cumulative summaries + shorter edge (ST-5.1) | about 0.6 to 0.75 µs |
| `aggregate`, ladder bucket | one row of the level | O(1) |
| bar over an arbitrary range | points, sparse tables, cumulative sums (ST-5.3) | 0.7 to 3.9 µs |
| range min, max, argmin, argmax | sparse table + edges | 0.2 to 1.3 µs |
| range median, quantile, count in `[lo, hi)` | wavelet matrix | 0.3 to 0.9 µs |
| any of the above with a receipt filter | the same, corrected by the revision table (ST-5.9) | + O(log R + entries) |
| any of the above on an adjusted series (split, total return, continuous future) | piecewise between events (ST-5.10) | + O(p) lookups; quantiles with more than 8 pieces through a run-scoped matrix |
| `subscribe` (replay) | one cursor per live instrument holding its current decoded block; at each label, the instruments alive and present advance; revisions merged at their receipt (ST-5.9) | each block decoded once; O(alive) per label |

---

## 7. Staging

Data moves from a vendor to the hot store through seven stages. Each stage's output is immutable; a failure leaves the published version untouched.

| Stage | Input | Output | Invariant checked |
| --- | --- | --- | --- |
| S0 Land | vendor objects: NDLake year files and registry, Databento files listed in `manifest.csv`, the live gateway | the landing log: (source, key, ETag or SHA-256, size, fetched at) | the object's checksum matches the vendor's manifest where one exists (Databento's `manifest.csv` carries SHA-256) |
| S1 Normalise | landed objects | Arrow batches in local staging (NVMe), one per (relation, instrument range), sorted by (instrument, time): ids from the registry, values as exact integers with their source exponent, receipt times | schema conformance and identity (D-8); every value converts exactly (ST-3) or is listed |
| S2 Classify | normalised batches and the current version | per instrument: appends (time after its last row), duplicates (identical to stored), revisions (an existing row with a different value), late rows (inside the tail's span) | duplicates dropped; counts per class reported |
| S3 Build | appends, revisions | appends applied to tails (ST-8); revisions: every affected block re-encoded copy-on-write, its block summaries, the cumulative summaries and cross-moments from that block to the instrument's end, the sparse table of the instrument, the ladder buckets containing it, presence, wavelets of the affected instrument and level, and one revision-table entry per superseded value (ST-5.9) | — |
| S4 Verify | built blocks | a verification report | every new block decodes to its source bit for bit (Prop. 3.2); summaries of new blocks equal a direct recomputation; for a sample of ladder buckets, the bar equals the bar recomputed from base rows (Prop. 5.2); for a sample of random ranges of every rebuilt instrument, every range query of ST-6.3 equals a direct reduction of the decoded rows; row counts per instrument equal the source's |
| S5 Publish | verified segments | a new version manifest (the list of segment hashes and the sequence high-water), written then renamed into place | atomic: a reader sees the old manifest or the new one; readers holding the old version keep it (pinning, D-3.4) |
| S6 Export and collect | published segments | the cold copy in S3 (pcodec-compressed segments, or Parquet for interoperability); unpinned superseded segments deleted | the cold copy round-trips to the hot form |

### 7.1 Versions and structural sharing

A version is identified by the hash of its logical content: its blocks' hashes in (relation, column, level, instrument, block index) order, with the revision table and the sequence high-water. Its manifest lists the segment files that currently hold it. A segment holds one (relation, column, level) for a contiguous instrument range, about 64 MB. Re-encoding a block writes a new copy of its segment; every other segment is shared with the previous version by hash. A version therefore costs the segments it changed, and the hot memory of two live versions is their union.

**Proposition 7.1 [N] (versions are immutable and cheap).** A published version's values never change, and the memory added by publishing a version is the size of the segments it does not share with versions already resident. *Proof.* Segments are content-addressed and never rewritten (S3, S5); a manifest names hashes; two manifests naming one hash map the same file. Consolidation (ST-8.7) writes new segments holding the same blocks, so it changes a version's layout and not its id. ∎ Bindings and pins name version ids, so they follow a version through consolidation, and an old layout is freed as soon as no reader maps it.

### 7.2 Build cost

Encoding costs 10 to 11 ns per value (ST-2.4), dominated by the GCD. A full build of Norgate and Databento (about 3.3B values) takes about 35 s on one thread, and is parallel by instrument range; the ladder, summaries, presence and wavelets roughly double it. A full rebuild on the reference machine is therefore under a minute; the GCD can be made cheaper by first testing the previous block's quantum, which nearly always divides. A daily Norgate update (about 20,000 rows of five columns) is a few milliseconds of appends.

### 7.3 Where staging runs

S0 to S5 run inside the data layer's process on a staging thread pool; staging batches live on local NVMe under `staging/` and are deleted after S5. Staging never blocks readers: queries and streams read the published version throughout. At most about 1 GB of staging memory is in use at a time (one instrument range's batches and the blocks being built).

### 7.4 The sizing run (G-ST2)

Before the store serves its first query at M-2, S0 to S4 run once over the full Norgate and Databento history in measure mode: every block is encoded and verified, every structure of ST-5 is built, and the bytes of each line of ST-9 are counted, the intraday ladder separately for outright contracts and for spreads. Inputs: the NDLake year files (4.2 GB) and the 117,677 Databento files (6.9 GB) from S3. Compute: about a minute on the M4's 10 cores (ST-7.2); the download dominates. The measured table is recorded in `docs/assessment.md` and replaces ST-9's estimates. If the store exceeds 7.2 GB, or a memory profile's peak exceeds its budget (ST-9.2), the shedding order of ST-9.2 applies until both fit. The sizing run is also the whole-store round trip of ST-13.

### 7.5 Retention and reproducibility

A run is reproducible while the versions its binding names can be read (A-4.2). The default (the owner's to change, ST-14):

| What | Kept |
| --- | --- |
| S0's landed vendor objects | indefinitely, in S3 (about 11 GB for both vendors today) |
| The cold copy of a version (S6) | while any run manifest, plan or claim names the version, and at least 90 days |
| Hot versions | the current version and every pinned version (D-3.4) |

A run on a version that is no longer hot is reproduced by restoring that version's cold segments (a sequential read, minutes), not by rebuilding it; rebuilding from S0's objects is the last resort, and it is deterministic because S1 to S4 are.

---

## 8. Appends

### 8.1 What an append is

An append is a row of an instrument whose time is after the instrument's last stored row at that level's base calendar. Every other change is a revision (S2), handled by copy-on-write and a new version. Live minute bars and the daily Norgate update are appends; vendor corrections of history are revisions.

### 8.2 Visibility by sequence number

Every append carries a sequence number (D-3.3). Per instrument and level, the append log records (sequence, visible row count) whenever the count changes, and the receipt time of each appended row. A reader with `as_of_seq = N` sees, for each instrument, the rows up to the visible count at the last entry with sequence ≤ N.

**Proposition 8.1 [N] (snapshot exactness under appends).** A reader at sequence N sees exactly the rows with sequence ≤ N, and no later append changes what it has read. *Proof.* Appends to an instrument are in time order and each extends its rows by a suffix, so the rows visible at N are a prefix of the instrument's rows, of the length the log records at N. Stored rows are never modified in place by an append: an append writes beyond the visible prefix of every reader at an earlier sequence, and the structures maintained in place are not consulted by such readers (8.3). A tail re-encoded for a late row is a new tail, the old one kept for its readers. Revisions do not modify stored rows either; they create a version (S3, S5). ∎ A binding (D-7.5) names a version and a sequence, and Proposition A-4.2 holds.

### 8.3 The tail block

Each instrument's last block per column is its tail: its values are packed horizontally (value `j` at bits `[j·W, (j+1)·W)`) with the block's base, quantum and width. An append writes the new offset in place when it fits: the value is on the block's grid (`(i − β) mod g = 0` at the block's exponent) and `0 ≤ (i − β)/g < 2^W`. Otherwise the tail is re-encoded with the new value (at most 1,023 values, about 10 µs) into a new tail, and the directory's tail pointer is swapped; readers at earlier sequences keep the old tail, which is freed when no reader holds it (an epoch counter per tail). When the tail reaches 1,024 values it is sealed: re-encoded in the lane-interleaved layout with base, quantum and width chosen afresh, appended to the instrument's run, and a new empty tail started.

Base segments are immutable, so a sealed block is written to the current **append segment** of its (relation, column, level): a file written sequentially in seal order, instruments interleaved, rotated at 64 MB or at the end of each trading day. The block is written and synced, then its **seal record** (instrument, column, level, block index, locator, the sequence of its last row, its summaries) is appended to the seal journal and synced with the log's group commit (D-3.1); the instrument's block table, cumulative summaries and sparse table are extended (8.4), and only then do readers see the sealed block. Live Databento seals about 1.35 blocks a day per active instrument and column (1,380 minute bars a day), so about 70,000 blocks and 100 MB a day for 10,000 instruments; Norgate seals a block per instrument every 1,024 trading days.

There is one writer per instrument (the data layer's ingest thread for its shard). An append is published only after its log group commit (at most 10 ms live, X-2.1), so no reader sees a row a crash could lose. The writer stores the value, then publishes the new visible count and its append-log entry with release ordering; readers load the count with acquire ordering and never read past it. Everything the writer maintains in place (the tail's running summary, the open ladder buckets, presence bits beyond a reader's count) therefore reflects the latest sequence only, and a reader at an earlier sequence does not use it: it reduces the tail's visible prefix directly (at most 1,023 values, under 0.5 µs), assembles an open bucket from the sealed buckets of the next finer level inside it and, recursively, that level's open bucket down to the base level's visible rows, and treats a set presence bit whose row is at or beyond its visible count as absent.

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

The feed decoder writes each minute bar to the log (D-5.4), then appends it to its instrument's tails: about 50,000 appends a minute for 10,000 instruments and five columns, a few milliseconds at about 100 ns an append. The minute barrier then seals the minute in every open bucket of the ladder that ends with it. A late row (a minute whose bar arrives after its barrier) is an append when it is still the instrument's latest row; when it falls inside the tail's span it is inserted by re-encoding the tail under a new sequence, and readers at earlier sequences keep their view (8.3); when it falls inside a sealed block it is a revision (S2 to S5), recorded in the revision table as a row absent before its receipt (ST-5.9). Ladder buckets it touches that were already sealed are revised the same way. Its effect on the kernel is the late-data rule (K-4.4).

### 8.6 The daily Norgate path

When NDLake's watermark advances, S0 lands the changed year file, S1 normalises it, and S2 compares it with the store: the new day's rows are appends; any change to an earlier row is a revision. Appends go through 8.3 and are visible immediately at their sequence; revisions are built, verified and published as a new version (S3 to S5).

### 8.7 Consolidation

Append segments scatter an instrument's recent blocks across files. Correctness does not suffer, since block tables address every block in O(1), but locality does: a long range read jumps between files. Consolidation restores it, and it is the store's only compaction. When the append segments of a (relation, column, level) exceed 512 MB, and at least weekly, a background task rewrites, one instrument range at a time, the base segment followed by that range's blocks from the append segments, in instrument order, as a new base segment. Blocks, headers and summaries are copied byte for byte: nothing is re-encoded, so block hashes, the version's id (ST-7.1) and Proposition 3.2 carry over. The new layout is checked by block hashes (S4) and published atomically (S5); readers that mapped the old layout keep it until they release it, and append segments are deleted by S6 when no layout names them. Cost: a copy at memory bandwidth, a few seconds for a column family; the extra memory at any moment is one segment (64 MB) and the append segments being folded. Revisions leave superseded segments behind, collected by S6 under ST-7.5.

### 8.8 Durability and restart

| State | Durable when | Where |
| --- | --- | --- |
| an appended row | its log group commit (D-3.1) | the log |
| a sealed block | its seal record is synced | an append segment and the seal journal |
| a version (staged, revised, consolidated) | its manifest is renamed into place after its segments are synced | the manifest |
| revision-table entries | with their version | a segment |
| a tail | never on its own; it is a function of the log | memory |

A seal record is written only after the log rows its block holds are durable, and a block becomes visible only after its record is, so the durable state is always the manifest, the journal and the log, in that order of precedence. Restart:

1. Open the latest manifest and map its segments.
2. Replay the seal journal past the manifest's high-water: journaled blocks join their block tables; bytes of append segments past the last record are truncated (a crash between writing a block and its record).
3. Rebuild every tail from the log: per instrument, the rows after its last sealed block, at most 1,023 per (column, level); about 50M rows in the worst case of 10,000 live instruments, a sequential log read and about 100 ns an append, under 10 s.
4. Prefault and lock the hot store (ST-9.1), 2 to 4 s from NVMe.
5. Resume subscriptions from their acknowledged sequences (D-5.6).

Staging work in flight (S0 to S4) is discarded and rerun: S0 is idempotent by checksum and S2 classifies against the current version. Target: serving within 30 s of start.

**Proposition 8.2 [N] (recovery).** After a restart, every query at every sequence up to the durable high-water returns what it returned before the crash. *Proof.* Readers saw only rows after their log commit and sealed blocks after their seal record (8.3); both are durable, and tails are rebuilt from the log by the same appends (a deterministic function of the rows); journaled blocks are the same bytes; truncated bytes were never visible. Versions are published by an atomic rename (S5). ∎


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
| Cumulative cross-moments of bar relations (`Σ C·V`, `Σ (H+L+C)·V`) | 0.03 GB |
| Directory and block tables (40 B for each of about 4.2M blocks) | 0.17 GB |
| Catalog structures, interval trees and standing event aggregates | 0.1 GB |
| Revision table | 0.01 GB |
| Event-interval summaries (ST-5.10) | 0.05 GB |
| Wavelet matrices, close and volume (ST-5.7) | 0.75 GB |
| Append logs and live tails | 0.5 GB |
| **Data layer, hot** | **≈ 6.8 to 7.2 GB** |
| Staging, while it runs | ≤ 1 GB |
| Kernels (one MW14-class program < 1 GB; a study of four kernels 2 to 4 GB) | ≤ 4 GB |
| Run-scoped on-demand structures (rolling results, a non-standard calendar, a wavelet built on demand, the decoded-block cache) | ≤ 2 GB |
| **Peak** (staging during a four-kernel study; `dedicated` profile) | **≈ 14 GB**, inside 16 GB with 2 GB for the operating system |
| **Typical** (one kernel, no staging) | **≈ 8.5 GB** |

The figures scale measured bytes per value by estimated row counts (±25 %). The intraday ladder's range is the one real unknown: a contract that trades every minute has a fifth as many 5-minute bars as minute bars, but a calendar spread that trades a few times an hour has nearly as many, and spreads are 76 % of CL's rows. M-2 measures it on the full history; if the store exceeds 7.2 GB, the shedding order of 9.2 applies.

### 9.1 Residency

Hot means resident. A mapped page the operating system has dropped costs a read from NVMe, tens of microseconds, on its next touch, which would break every figure of ST-6. So the data layer, at start and after each publication, prefaults every segment of the current version (a sequential read, 2 to 4 s for 7 GB) and locks it in memory (`mlock`; on Linux `MAP_POPULATE` does the prefault). The kernel's mappings of the same files (ST-10.2) share the locked pages. If the operating system refuses the lock (a limit on locked memory), the data layer runs unlocked, reports the shortfall, and counts major page faults in `stats`; the deployment document raises the limit, and G10 and the performance gates of ST-13 run only with the store locked. Run-scoped structures are ordinary memory and are not locked.

### 9.2 Memory profiles and the shedding order

| Profile | For | Operating system and other programs | abt at peak | Policy |
| --- | --- | --- | --- | --- |
| `dedicated` | a machine that runs only abt (live, G10) | about 2 GB | about 14 GB | the table above |
| `workstation` | the owner's M4 with a desktop session (the default until a dedicated machine exists) | 4 to 5 GB | about 11 GB | at most two kernels at a time (≤ 2 GB), staging only while no study runs, run-scoped structures ≤ 1.5 GB: 7.2 + 2 + 1.5 ≈ 10.7 GB |

The profile is the data layer's configuration; the query layer's plan executor (Q-2) reads the kernel limit from `stats` and queues kernels beyond it. If the measured store (ST-7.4) or a profile's peak exceeds its budget, precomputed structures are shed in this order, each becoming built on first demand in run scope:

| Order | Structure shed | Saves |
| --- | --- | --- |
| 1 | the decoded-block cache and on-demand matrices, to their minimum | up to 1 GB of run scope |
| 2 | the Databento 5-minute level | 0.4 to 0.6 GB |
| 3 | wavelet matrices of volume | about 0.35 GB |
| 4 | the Databento 15-minute level | 0.15 to 0.2 GB |
| 5 | wavelet matrices of close | about 0.4 GB |

Shedding changes speed and never answers: every shed structure is rebuilt exactly from the levels below it (Propositions 3.2 and 5.2).

---

## 10. Interfaces

### 10.1 `abt-codec`

A fourth shared crate beside `abt-wire` and `abt-agg` (P-1): the block format of ST-4 (encode, decode, single-value extraction, the tail), prediction, the summaries of ST-5.1 and ST-5.2, rank and select, wavelet matrices. Pure functions over byte slices, no I/O, no semantics; property-tested for Proposition 3.2 on random and real blocks. The data layer uses it to build and query; the kernel uses it to read mapped segments (10.2); `abt-agg`'s reductions run on its decoded blocks.

### 10.2 Mapping

Published segments are immutable files on local NVMe, so the data layer can hand a reader the file instead of its bytes. The data service gains one request:

| Request | Fields | Response |
| --- | --- | --- |
| `map` | `View`, relation, columns, level, instrument range | a table of segment descriptors (base and append segments): path, offset, length, codec version; the block tables' locations, covering only journaled blocks (ST-8.3); and the tail descriptors with their visible counts at the view's sequence |

The reader maps the segments read-only and decodes with `abt-codec`. Tails and append logs are read through `query` and `subscribe` as before, since they change. A reader on another machine uses `query` and `subscribe` only. The data layer remains the single writer of every file (A-D20).

### 10.3 Determinism

Decoding is exact (Prop. 3.2), sums are exact (Prop. 3.3), windows are exact (Prop. 5.1), ladder bars are exact (Prop. 5.2), and visibility is by sequence (Prop. 8.1), so Propositions A-4.2 (reproduction) and P-5.1 (transport invisibility) hold for mapped reads as for streamed ones.

### 10.4 Ticks on the wire

Tick columns cross the protocol as Arrow `Decimal128(18, s)`: each value is the integer number of units of `10^(−s)`, with `s` the finest exponent among the blocks in the batch (at most 9). Precision 18 guarantees every value fits an `i64` (prices below 10^9 at `s = 9`), so a reader may hold the column as `i64` with its scale. A request receives tick columns when it asks for `numerics = exact` (or `ticks = true` on `query`), and the `Float64` view otherwise. The kernel's and the data layer's exact aggregates then consume the same integers through `abt-agg`, so Proposition P-1.1 (one implementation, one answer) holds in `exact` mode as in the others. Adjusted views (ST-5.10) cross as `Float64` unless every factor in the request is rational (splits, additive rolls), in which case they cross as `Decimal128` at the scale that holds them exactly, or the request is refused with that scale.

---

## 11. ORATS

ORATS is not held hot. Measured on three consecutive days of 2025 and scaled to about 2.06B rows: its market data (contract structure, quotes, volume and open interest) needs about 9 GB at the smallest lossless encoding, and its vendor model columns (implied volatilities, smoothed volatility, theoretical values and greeks) about 78 GB at the smallest lossless encoding and about 100 GB in a fast one. The model columns do not follow Black-Scholes on `smoothSmvVol` (median relative errors 1 to 4 %), so they cannot be recomputed. Holding ORATS is the subject of a separate decision; the demand-driven design (bound chains loaded hot before a run, full history on NVMe in pcodec chain-day blocks) is the recommendation on record.

---

## 12. Amendments to the other documents

| Document | Change |
| --- | --- |
| `00-architecture.md` | A-D35 to A-D46 below; targets "storage per stored value" (≤ 2 B per value resident on average with every precomputed structure, about 6.8 to 7.2 GB for Norgate and Databento, locked), peak by memory profile, and restart to serving ≤ 30 s; the open decisions of ST-14 in its section 8 |
| `01-data.md` | D-2.1: prices and quantities are tick columns (ST-3), `f64` is a view; D-3.1 and D-3.2: the resident form is ST-4, partitions are ST-7 segments; the log, versions, snapshot reads, the cache and the tiers stand |
| `04-protocol.md` | `abt-codec` beside `abt-wire` and `abt-agg`; the `map` request in the data service |
| `06-performance.md` | X-2.1's store structures are superseded by ST-4 and ST-5 for Norgate and Databento |
| `05-migration.md` | M-2 builds `abt-codec`, the staging pipeline and the hot store; the encoding studies are its first benchmarks; G-ST1 and G-ST2 are its entry gates |
| `02-kernel.md` | the `numerics` field gains `exact` (K-2.2) |
| `01-data.md`, `04-protocol.md` | the scalar type `Tick` (`Decimal128(18, s)`, ST-10.4) |

| # | Decision | Rejected alternative, and why |
| --- | --- | --- |
| A-D35 | Prices and quantities are integer ticks with a per-block quantum (exponent × GCD); floats are views | `f64` (or `f32`) storage: sums are not associative, Databento's ticks are not `f32`, and the GCD saves 5 to 19 bits a value on binary-fraction ticks (ES's 0.25 to ZT's 1/256) |
| A-D36 | The resident form is instrument-contiguous, lane-interleaved blocks with OHLC prediction and patched exceptions, memory-mapped | uncompressed columns (about 26 GB and slower); entropy coding in memory (ten to twenty times slower to decode) |
| A-D37 | Everything program-independent is precomputed and hot (ST-5); program-dependent results are recomputed in run-scoped memory | caching rolling results: unbounded and incompressible |
| A-D38 | A seven-stage staging pipeline with verification by decoding, atomic publication and content-addressed segments shared between versions | rebuilding the store per update: minutes instead of milliseconds, and no structural sharing |
| A-D39 | Appends in place into tail blocks, visible by sequence number; revisions as copy-on-write versions | appends as new versions: a version per minute live |
| A-D40 | ORATS is not hot; its holding is decided separately | any hot ORATS design: at least about 87 GB losslessly |
| A-D41 | Durability from the log and a seal journal; tails are rebuilt from the log on restart (ST-8.8) | persisting tails in place: torn writes, and a second source of truth beside the log |
| A-D42 | Sealed blocks go to append segments; consolidation, the only compaction, copies them into instrument order byte for byte; version ids are logical, so pins follow a version across layouts (ST-8.7, ST-7.1) | writing into base segments in place (breaks immutability and mapped readers); never consolidating (locality decays every day) |
| A-D43 | Adjusted series are views evaluated piecewise between events, over precomputed event-interval summaries (ST-5.10) | stored adjusted copies: one per adjustment, and every split or dividend rewrites the instrument's history |
| A-D44 | Ticks cross the protocol as Arrow `Decimal128(18, s)` (ST-10.4) | `Float64` only: `exact` mode could not cross layers; `Int64` with scale metadata: not a standard Arrow type |
| A-D45 | Hot means locked: prefault and `mlock`, memory profiles and a shedding order (ST-9.1, ST-9.2) | relying on the page cache: a dropped page costs tens of microseconds and no error |
| A-D46 | Futures calendars: UTC-aligned intraday buckets, sessions and days by trade date, every receipt at its bucket's end (ST-5.3) | exchange-local alignment (identical for CME and ICE at whole-hour offsets, but it puts time-zone data on the hot path); UTC days (they cut every Globex session in two) |

---

## 13. Tests and gates

| Test | Statement |
| --- | --- |
| `abt-codec` property tests | encode then decode is the identity on random blocks at every width 0 to 64, with and without exceptions, with `recip_exact` set and unset; single-value extraction equals the decoded block |
| Real-data round trip | every block of the staged Norgate and Databento store decodes bit for bit to the vendor's values (S4 over the whole store, once, at M-2) |
| Exact aggregates | sums, moments and windows from cumulative summaries equal a 128-bit rational reference on random windows |
| Ladder | every precomputed bar of a sample of instruments equals the bar recomputed from base rows |
| Ranges | for every query of ST-6.3, on random ranges of random instruments, including ranges of one row, ranges that start or end mid-block, mid-row and at block boundaries, ranges that cover exceptions, ranges that reach the tail, and ranges read at earlier sequences and with receipt filters over revised rows: the answer equals a direct reduction of the decoded rows (exactly for every exact form) |
| Appends | interleaved appends and reads at random sequences: every reader sees exactly the rows with sequence ≤ its own (Prop. 8.1); tails re-encoded under readers keep the readers' views |
| Revisions | a revision publishes a version whose unchanged segments are shared by hash, and readers of the old version are unaffected |
| Performance | `scripts/study/encoding/` benchmarks rerun on the reference machine at M-2 and M-8: decode-and-sum at or below the raw `u32` scan at every width up to 27; range reads of 1,000 rows or more at or below an uncompressed scan; range max, OHLCV bars, medians and counts within twice the figures of ST-2.4; the hot store within the budget of ST-9 |
| Parity | G1 unchanged: MW14 and R8L read their bars through the new store in `two_pass` mode and reproduce their references |
| Recovery | the process is killed at random points during appends, seals, consolidations and staging; after restart, every answer at every durable sequence equals the answer before the crash (Prop. 8.2), and no unjournaled block is visible |
| Consolidation | every answer is identical before and after; the version id is unchanged; the extra memory never exceeds one segment plus the append segments being folded |
| Adjusted ranges | split, total-return and continuous-futures views over random ranges crossing events: sums exact for splits and additive adjustments and equal to the ordered definition for total return, extremes and quantiles equal to a direct reduction of the adjusted decoded rows, in both layers bit for bit |
| Calendars | weeks containing daylight-saving changes, Sunday-evening sessions, early closes, holidays and session-table changes: every ladder bar equals its bucket recomputed from base rows under ST-5.3's rules, with its receipt at the bucket's end |
| Ticks on the wire | `Decimal128` round trip of every staged value; `exact` aggregates computed by the kernel equal the data layer's on the same rows |
| Residency | with the store locked, major page faults stay at zero through a G10 run after warm-up |
| G-ST1 (entry to M-2's store work) | on the M4 with the full store built and locked: decode and sum on real blocks of mixed widths within 1.5 times the single-width figure of ST-2.4 at the same mean width; OHLC reconstruction and sum within 1 ns per bar per column; points within 100 ns in a batch; the range figures of ST-2.4 within twice; a 10-thread scan at least 6 times one thread. If it fails, in order: blocks of one width per instrument-column-year (fewer dispatches, slightly larger); O, H, L stored unpredicted (10 to 30 % larger); a decoded copy of the hottest columns within the shedding budget of ST-9.2 |
| G-ST2 | the sizing run of ST-7.4: measured sizes recorded, and the store within ST-9's budget after shedding |

---

## 14. Open decisions and the defaults that stand

These are the owner's; until each is decided, the default is what the spec builds. They are also listed in `00-architecture.md` section 8.

| Decision | Default | Where |
| --- | --- | --- |
| `exact` numerics as the default for new programs | `exact` for programs written after M-3; `two_pass` pinned for MW14 and R8L so G1 holds | ST-3, D-6.3, K-2.2 |
| How ORATS is held | not hot; demand-driven: the chains a run binds are loaded hot before it, the full history lives on NVMe in pcodec chain-day blocks | ST-11 |
| Retention | ST-7.5: vendor objects kept in S3 indefinitely, cold copies of every version a run, plan or claim names | ST-7.5 |
| The reference machine's memory profile | `workstation` until a dedicated machine exists | ST-9.2 |
| Vendor price lists, futures margin source, an options book with known results, S3 as a resolver tier | as `00-architecture.md` section 8; S6 already stores cold copies in S3, while S3 as a tier the resolver consults remains open | A-8 |

---

## 15. What is established, new and decided in this document

- [E] Frame-of-reference and patched frame-of-reference coding (Zukowski, Héman, Nes and Boncz 2006); the FastLanes layout (Afroozeh and Boncz 2023); ALP's verified decimal reconstruction (Afroozeh, Kuffó and Boncz 2023); rank and select directories (Jacobson 1989; Vigna 2008); wavelet matrices and range quantiles (Claude, Navarro and Ordóñez 2015; Gagie, Puglisi and Turpin 2009); roaring bitmaps (Lemire et al. 2016); sparse tables for range minima (Bender and Farach-Colton 2000); augmented interval trees (Cormen, Leiserson, Rivest and Stein, ch. 14); pcodec (Loncaric 2024).
- [N] Lossless representation (3.2); exact sums across blocks of different quanta (3.3); exact windows from cumulative summaries (5.1); exact ladder (5.2); range closure of every encoding and structure (6.1); immutable versions with structural sharing and logical identity (7.1); snapshot exactness under appends (8.1); recovery (8.2).
- [D] A-D35 to A-D46.
