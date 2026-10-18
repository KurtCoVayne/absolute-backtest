"""Prediction before packing, measured with the fast format (per block: exponent + GCD + base +
bit width, deltas or offsets): (1) OHLC from the close; (2) ORATS from the same contract's previous
day and the ask from the bid; (3) zero-heavy integers as a presence bitmap + packed non-zeros."""
import sys, glob
import numpy as np, pyarrow as pa, pyarrow.parquet as pq, pyarrow.compute as pc

ROOT = sys.argv[1]; B = 1024; HDR = 128


def bits(v):
    v = np.asarray(v, np.float64); w = np.zeros(v.shape, np.int64); nz = v > 0
    w[nz] = np.floor(np.log2(v[nz])).astype(np.int64) + 1; return w


def packed_bpv(x, starts=None):
    """Best of FOR and delta per block, with the block GCD quantum; x are integers."""
    x = np.asarray(x, np.int64); n = len(x)
    if n == 0: return 0.0
    st = np.arange(0, n, B) if starts is None else starts
    lens = np.diff(np.concatenate([st, [n]]))
    mn = np.minimum.reduceat(x, st); off = x - np.repeat(mn, lens)
    g = np.gcd.reduceat(off, st); g[g == 0] = 1; off = off // np.repeat(g, lens)
    w_for = bits(np.maximum.reduceat(off, st))
    d = np.diff(off, prepend=off[:1]); d[st] = 0; zz = np.where(d >= 0, 2 * d, -2 * d - 1)
    w_d = bits(np.maximum.reduceat(zz, st))
    return float((lens * np.minimum(w_for, w_d)).sum() + len(st) * HDR) / 8 / n


def starts_of(ent):
    change = np.flatnonzero(ent[1:] != ent[:-1]) + 1
    s0 = np.concatenate([[0], change]); e0 = np.concatenate([change, [len(ent)]])
    return np.concatenate([np.arange(a, b, B) for a, b in zip(s0, e0)])


def ints(x, e):
    return np.round(np.asarray(x, np.float64) * 10.0 ** e).astype(np.int64)


def sparse_bpv(x):
    """Zero-heavy integers: a presence bitmap (1 bit per row) + the non-zeros packed."""
    x = np.asarray(x, np.int64); nz = x[x != 0]
    return (len(x) / 8 + packed_bpv(nz) * len(nz)) / len(x)


print("== OHLC from the close (bytes per value, fast format)")
for label, path, ent_col, time_col, e in (("ndlake 2024", f"{ROOT}/ndlake/2024.parquet", "asset_id", "date", 4),
                                         ("ndlake 1995", f"{ROOT}/ndlake/1995.parquet", "asset_id", "date", 4)):
    t = pq.read_table(path, columns=[ent_col, time_col, "open", "high", "low", "close", "series_type"])
    t = t.filter(pc.equal(t["series_type"], "equity")).sort_by([(ent_col, "ascending"), (time_col, "ascending")])
    o, h, l, c = (ints(np.nan_to_num(t[k].to_numpy(zero_copy_only=False)), e) for k in ("open", "high", "low", "close"))
    ent = t[ent_col].to_numpy(); st = starts_of(ent)
    prev_c = np.concatenate([[c[0]], c[:-1]]); prev_c[np.concatenate([[True], ent[1:] != ent[:-1]])] = o[np.concatenate([[True], ent[1:] != ent[:-1]])]
    raw = sum(packed_bpv(v, st) for v in (o, h, l, c)) / 4
    pred = (packed_bpv(c, st) + packed_bpv(h - np.maximum(o, c), st) + packed_bpv(np.minimum(o, c) - l, st) + packed_bpv(o - prev_c, st)) / 4
    print(f"  {label}: OHLC independent {raw:.3f} B/v -> close + (high-max(o,c)) + (min(o,c)-low) + (open-prev close) {pred:.3f} B/v")

for root in ("ES", "CL", "ZN", "GC"):
    t = pa.concat_tables([pq.read_table(f) for f in sorted(glob.glob(f"{ROOT}/dbn/{root}_*.parquet"))]).sort_by([("instrument_id", "ascending"), ("ts_event", "ascending")])
    o, h, l, c = (ints(t[k].to_numpy(), 7) for k in ("open", "high", "low", "close"))
    ent = t["instrument_id"].to_numpy().astype(np.int64); st = starts_of(ent)
    first = np.concatenate([[True], ent[1:] != ent[:-1]]); prev_c = np.concatenate([[c[0]], c[:-1]]); prev_c[first] = o[first]
    raw = sum(packed_bpv(v, st) for v in (o, h, l, c)) / 4
    pred = (packed_bpv(c, st) + packed_bpv(h - np.maximum(o, c), st) + packed_bpv(np.minimum(o, c) - l, st) + packed_bpv(o - prev_c, st)) / 4
    v = t["volume"].to_numpy().astype(np.int64)
    print(f"  databento {root}: OHLC independent {raw:.3f} -> predicted {pred:.3f} B/v; volume {packed_bpv(v, st):.3f} B/v")

print("== ORATS: previous day of the same contract, ask from bid, sparse integers")
cols = ["ticker", "expirDate", "strike", "cBidPx", "cAskPx", "pBidPx", "pAskPx", "cVolu", "pVolu", "cOi", "pOi", "cMidIv", "smoothSmvVol", "delta", "gamma", "vega", "theta", "cValue"]
days = [pq.read_table(f, columns=cols).sort_by([("ticker", "ascending"), ("expirDate", "ascending"), ("strike", "ascending")]) for f in sorted(glob.glob(f"{ROOT}/orats/*.parquet"))]
def key(t): return pc.binary_join_element_wise(t["ticker"], pc.cast(t["expirDate"], pa.string()), pc.cast(t["strike"], pa.string()), "|").to_numpy(zero_copy_only=False)
k1, k2 = key(days[1]), key(days[2])
idx = {k: i for i, k in enumerate(k1)}
pos = np.array([idx.get(k, -1) for k in k2]); have = pos >= 0
print(f"  day 3 contracts also present on day 2: {have.mean():.4f}")
E = {"cBidPx": 2, "cAskPx": 2, "pBidPx": 2, "pAskPx": 2, "cMidIv": 5, "smoothSmvVol": 6, "delta": 6, "gamma": 8, "vega": 8, "theta": 8, "cValue": 2}
for c in ("cBidPx", "cAskPx", "cMidIv", "smoothSmvVol", "delta", "gamma", "vega", "theta", "cValue"):
    cur = ints(np.nan_to_num(days[2][c].to_numpy(zero_copy_only=False)), E[c]); prv = ints(np.nan_to_num(days[1][c].to_numpy(zero_copy_only=False)), E[c])
    resid = cur.copy(); resid[have] = cur[have] - prv[pos[have]]
    print(f"  {c:<13} raw {packed_bpv(cur):.3f} B/v   vs previous day {packed_bpv(resid):.3f} B/v   unchanged share {np.mean(resid[have] == 0):.3f}")
cb = ints(days[2]["cBidPx"].to_numpy(zero_copy_only=False), 2); ca = ints(days[2]["cAskPx"].to_numpy(zero_copy_only=False), 2)
print(f"  cAskPx as (ask - bid): {packed_bpv(ca - cb):.3f} B/v (raw {packed_bpv(ca):.3f})")
for c in ("cVolu", "pVolu", "cOi", "pOi"):
    cur = days[2][c].to_numpy().astype(np.int64); prv = days[1][c].to_numpy().astype(np.int64)
    resid = cur.copy(); resid[have] = cur[have] - prv[pos[have]]
    print(f"  {c:<6} packed {packed_bpv(cur):.3f}  sparse {sparse_bpv(cur):.3f}  vs previous day sparse {sparse_bpv(resid):.3f} B/v  (zero share {np.mean(cur == 0):.2f})")
