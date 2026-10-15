# Encoding study (Oct 15, 2026)

The measurements behind `docs/spec/07-storage.md` (ST-2). Every number in ST-2 is produced by a script here, on samples of the company's own data in S3. The data is not committed; fetch the samples into one directory (`$DATA`) with the layout below.

## Samples

| Path under `$DATA` | Source |
| --- | --- |
| `ndlake/1995.parquet`, `ndlake/2024.parquet` | `s3://ndlake-094828274452/bars_by_date/year=1995/data.parquet` and `year=2024/data.parquet` |
| `dbn/{ES,ZN,ZT,CL,GC,6J}_{0303,0304}.parquet` | `s3://dbn-futures-094828274452/GLBX.MDP3/ohlcv-1m/<ROOT>.FUT/2025-03-03.parquet` and `2025-03-04.parquet` |
| `dbn/def/<ROOT>.parquet` | `s3://dbn-futures-094828274452/GLBX.MDP3/definition/<ROOT>.parquet` |
| `orats/2025030{3,4,5}.parquet` | `s3://orats-backup-094828274452/warehouse/year=2025/ORATS_SMV_Strikes_2025030{3,4,5}.parquet` (ST-11 only) |

Databento objects are checked against the SHA-256 column of the bucket's `manifest.csv`.

## Scripts

Python 3 with `numpy`, `pyarrow`, `pcodec`. Run with `python3 -I` (the samples are vendor files).

| Script | Measures | Output |
| --- | --- | --- |
| `python/encoding_study.py` | generic: per column, `f32` exactness, the decimal-exponent distribution, block widths (offsets, deltas, patched), Parquet sizes, instrument-contiguous against file order | JSON on stdout |
| `python/real_study.py $DATA` | the same on the samples, per source; writes the integer columns (`bench_*.i64`, `.starts`, `.exp`, `.f32`) for the Rust benches | `$DATA/study.json` (copy in `results/`) |
| `python/quantum_and_pco.py $DATA` | the block GCD quantum against a power of ten; pcodec sizes and decode speed; Databento tick round trip and rows without a definition | `$DATA/study2.json` (copy in `results/`) |
| `python/predict_study.py $DATA` | OHLC predicted from the close; ORATS against the previous day, ask from bid, sparse integers | stdout (figures in ST-2.3 and ST-11) |

## Rust benches

Each is a standalone crate (`cargo run --release -- …`), single-threaded, best of several runs.

| Crate | Measures | Arguments |
| --- | --- | --- |
| `codec-bench` | synthetic first pass: decimal + offsets, deltas, volume packing | none |
| `real-bench` | horizontal bit-packing with the block GCD on the real columns, fused decode + exact sum, exact `f32` rebuild, raw `f32`/`f64` scans | `$DATA` |
| `raw-bench` | uncompressed baselines on NDLake 2024 close: `i64` and canonical `f64` scans, rolling mean by sliding state and by prefix sums, exact variance in `i128`, random windows | `$DATA` |
| `lanes-bench` | the lane-interleaved (FastLanes) layout: decode + sum at widths 5 to 27 against raw `u32`, `i64`, `f64` scans; with `extra`, the encode cost and random windows from cumulative block sums with the shorter edge decoded | none, or `extra` |

## Results (2-core 2.8 GHz VM)

The tables of ST-2 are the results; `results/study.json` and `results/study2.json` are the raw outputs of the two Python studies. To be rerun on the reference machine at M-2 and M-8 (ST-13).
