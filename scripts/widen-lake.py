#!/usr/bin/env python3.12
"""scripts/widen-lake.py SRC DST K: a lake K times as wide.

Every asset of the lake SRC is written K times into DST: once as it is, and K-1 times as a clone with an asset
id of its own (id + c * 10,000,000), a ticker and a source symbol of its own (the prefix XQc: the Norgate
adapter reads a ticker's suffix, and renames a colliding ticker to its source symbol) and prices moved bar by
bar by up to 0.1% (one factor a bar for open, high, low, close and turnover, so a bar stays a bar). A clone is
a member of the lists its original is a member of, over the same days, with the same actions.

A stand-in for a universe the lake does not hold: with K = 2 the three S&P lists hold 3,000 names at a time,
the Russell 3000's size, over the same calendar and the same kinds of history. It measures how a run's time
and memory grow with the universe; its returns mean nothing (docs/handoff/benchmark.md, "A wider universe")."""
import glob, os, sys
import numpy as np, pandas as pd, pyarrow as pa, pyarrow.parquet as pq
src, dst, K = sys.argv[1], sys.argv[2], int(sys.argv[3])
OFFSET = 10_000_000
def clone(df, c, prices=False, seed=0):
    d = df.copy()
    assert d['asset_id'].max() < OFFSET
    d['asset_id'] = d['asset_id'] + c * OFFSET
    if 'ticker' in d.columns:
        d['ticker'] = f'XQ{c}' + d['ticker'].astype(str)
    if 'source_symbol' in d.columns:
        d['source_symbol'] = f'XQ{c}' + d['source_symbol'].astype(str)
    if 'name' in d.columns:
        d['name'] = d['name'].astype(str) + f' ({c})'
    if prices:
        f = 1.0 + 0.002 * (np.random.default_rng(seed * 100 + c).random(len(d)) - 0.5)
        for col in ('open', 'high', 'low', 'close'):
            d[col] = d[col] * f
        if 'turnover' in d.columns:
            d['turnover'] = d['turnover'] * f
    return d
n = 0
for path in sorted(glob.glob(os.path.join(src, '**', '*.parquet'), recursive=True)):
    rel = os.path.relpath(path, src)
    t = pq.read_table(path)
    df = t.to_pandas()
    bars = rel.startswith('curated/')
    seed = int(''.join(ch for ch in rel if ch.isdigit())[:4] or 0)
    out = pd.concat([df] + [clone(df, c, bars, seed) for c in range(1, K)], ignore_index=True)
    if bars:
        out = out.sort_values(['date', 'asset_id'], kind='stable')
    os.makedirs(os.path.dirname(os.path.join(dst, rel)), exist_ok=True)
    pq.write_table(pa.Table.from_pandas(out, schema=t.schema, preserve_index=False), os.path.join(dst, rel), compression='zstd')
    n += len(out)
for d in glob.glob(os.path.join(src, '*', '*')):
    os.makedirs(os.path.join(dst, os.path.relpath(d, src)), exist_ok=True)
print(f'lake x{K}: {n} rows written to {dst}')
