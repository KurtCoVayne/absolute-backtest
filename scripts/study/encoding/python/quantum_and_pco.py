"""Second pass: (a) quantum = the block's GCD (finds 1/64, 1/256, 0.25, 1/8 ...) instead of a power
of ten; (b) pcodec, a distribution-aware lossless codec, on the raw floats and on the integers,
with decompression timings; (c) zstd-on-bitpacked is not measured: decode speed is the goal."""
import sys, time, json, glob
import numpy as np, pyarrow as pa, pyarrow.parquet as pq, pyarrow.compute as pc
from pcodec import standalone, ChunkConfig

ROOT = sys.argv[1]; B = 1024; HDR = 128


def bits(v):
    v = np.asarray(v, np.float64); w = np.zeros(v.shape, np.int64); nz = v > 0
    w[nz] = np.floor(np.log2(v[nz])).astype(np.int64) + 1; return w


def starts_of(ent):
    change = np.flatnonzero(ent[1:] != ent[:-1]) + 1
    s0 = np.concatenate([[0], change]); e0 = np.concatenate([change, [len(ent)]])
    return np.concatenate([np.arange(a, b, B) for a, b in zip(s0, e0)])


def mem_bpv(ints, starts, gcd=False):
    n = len(ints); lens = np.diff(np.concatenate([starts, [n]]))
    mn = np.minimum.reduceat(ints, starts); off = ints - np.repeat(mn, lens)
    if gcd:
        g = np.gcd.reduceat(off, starts); g[g == 0] = 1; off = off // np.repeat(g, lens)
    else:
        g = np.ones(len(starts), np.int64)
    w_for = bits(np.maximum.reduceat(off, starts))
    d = np.diff(off, prepend=off[:1]); d[starts] = 0; zz = np.where(d >= 0, 2 * d, -2 * d - 1)
    w_d = bits(np.maximum.reduceat(zz, starts))
    nb = len(starts)
    return {"for": round(float((lens * w_for).sum() + nb * HDR) / 8 / n, 3), "delta": round(float((lens * w_d).sum() + nb * HDR) / 8 / n, 3),
            "w_for_p50": int(np.median(w_for)), "w_delta_p50": int(np.median(w_d)), "gcd_p50": int(np.median(g))}


def per_block_ints(x, starts, f32):
    """ALP-style per-block decimal exponent (exact for every value, as pass 1 showed)."""
    n = len(x); lens = np.diff(np.concatenate([starts, [n]])); xd = x.astype(np.float64)
    e = np.full(n, 10, np.int8); todo = np.ones(n, bool)
    for k in range(10):
        s = 10.0 ** k; idx = np.flatnonzero(todo); back = np.round(xd[idx] * s) / s
        ok = (back.astype(np.float32) == x[idx].astype(np.float32)) if f32 else (back == xd[idx])
        e[idx[ok]] = k; todo[idx[ok]] = False
    eb = np.maximum.reduceat(np.minimum(e, 9).astype(np.int64), starts)
    return np.round(xd * 10.0 ** np.repeat(eb, lens)).astype(np.int64)


def pco(arr, reps=5):
    arr = np.ascontiguousarray(arr)
    t = time.perf_counter(); comp = standalone.simple_compress(arr, ChunkConfig()); tc = time.perf_counter() - t
    best = 1e9
    for _ in range(reps):
        t = time.perf_counter(); back = standalone.simple_decompress(comp); best = min(best, time.perf_counter() - t)
    assert np.array_equal(back.view(np.uint8), arr.view(np.uint8)), "pcodec round trip differs"
    return {"bpv": round(len(comp) / len(arr), 3), "decode_ns_per_value": round(best / len(arr) * 1e9, 3), "encode_ns_per_value": round(tc / len(arr) * 1e9, 1)}


res = {}
# NDLake equities, instrument-contiguous.
for year in ("1995", "2024"):
    t = pq.read_table(f"{ROOT}/ndlake/{year}.parquet", columns=["asset_id", "date", "close", "volume", "series_type"])
    t = t.filter(pc.equal(t["series_type"], "equity")).sort_by([("asset_id", "ascending"), ("date", "ascending")])
    ent = t["asset_id"].to_numpy(); x = t["close"].to_numpy(); ok = ~np.isnan(x); ent, x = ent[ok], x[ok]
    st = starts_of(ent); ints = per_block_ints(x.astype(np.float32), st, True)
    vol = t["volume"].to_numpy()[ok].astype(np.int64)
    res[f"ndlake_{year}_close"] = {"decimal": mem_bpv(ints, st), "decimal+gcd": mem_bpv(ints, st, True),
                                   "pco_f32": pco(x.astype(np.float32)), "pco_ints": pco(ints)}
    res[f"ndlake_{year}_volume"] = {"for": mem_bpv(vol, st), "pco": pco(vol)}
    print(year, file=sys.stderr)

# Databento: decimal vs tick/GCD quantum, pcodec on f64 and on ticks.
defs = {}
for f in glob.glob(f"{ROOT}/dbn/def/*.parquet"):
    d = pq.read_table(f).to_pydict()
    for iid, tick in zip(d["instrument_id"], d["min_price_increment"]): defs[int(iid)] = tick
for root in ("ES", "ZN", "ZT", "CL", "GC", "6J"):
    t = pa.concat_tables([pq.read_table(f) for f in sorted(glob.glob(f"{ROOT}/dbn/{root}_*.parquet"))])
    t = t.sort_by([("instrument_id", "ascending"), ("ts_event", "ascending")])
    ent = t["instrument_id"].to_numpy().astype(np.int64); x = t["close"].to_numpy(); st = starts_of(ent)
    known = [defs[int(i)] for i in np.unique(ent) if int(i) in defs]
    root_tick = max(set(known), key=known.count)
    missing = np.mean([int(i) not in defs for i in ent])
    tick = np.array([defs.get(int(i), root_tick) for i in ent]); ticks = np.round(x / tick).astype(np.int64)
    exact_back = float(np.mean((ticks * tick) == x))
    ints = per_block_ints(x, st, False)
    res[f"dbn_{root}_close"] = {"decimal": mem_bpv(ints, st), "decimal+gcd": mem_bpv(ints, st, True), "tick": mem_bpv(ticks, st),
                                "tick_roundtrip_exact_share": exact_back, "rows_without_definition": round(float(missing), 4), "pco_f64": pco(x), "pco_ticks": pco(ticks)}
    v = t["volume"].to_numpy().astype(np.int64)
    res[f"dbn_{root}_volume"] = {"for": mem_bpv(v, st), "pco": pco(v)}
    print(root, file=sys.stderr)

if not glob.glob(f"{ROOT}/orats/*.parquet"):
    print("orats: no samples under orats/ (ST-11 only): skipped", file=sys.stderr)
    json.dump(res, open(f"{ROOT}/study2.json", "w"), indent=1)
    sys.exit(0)
# ORATS: one day, file order (the chains are ~24 rows: blocks of 1024 across chains).
day = pq.read_table(sorted(glob.glob(f"{ROOT}/orats/*.parquet"))[0])
day = day.sort_by([("ticker", "ascending"), ("expirDate", "ascending"), ("strike", "ascending")])
n = day.num_rows; st = np.arange(0, n, B)
for col in ("strike", "cBidPx", "cAskPx", "cValue", "cMidIv", "smoothSmvVol", "delta", "gamma", "vega", "theta", "cVolu", "cOi"):
    v = day[col].to_numpy(zero_copy_only=False)
    if v.dtype.kind == "f":
        v = np.nan_to_num(v.astype(np.float32), nan=0.0).astype(np.float32)
        ints = per_block_ints(v, st, True)
        res[f"orats_{col}"] = {"decimal": mem_bpv(ints, st), "decimal+gcd": mem_bpv(ints, st, True), "pco_f32": pco(v), "pco_ints": pco(ints)}
    else:
        v = v.astype(np.int64); res[f"orats_{col}"] = {"for": mem_bpv(v, st), "pco": pco(v)}
    print(col, file=sys.stderr)

json.dump(res, open(f"{ROOT}/study2.json", "w"), indent=1)
for k, v in res.items():
    print(f"{k:<24} " + "  ".join(f"{m}={json.dumps(r, separators=(',', ':'))}" for m, r in v.items()))
