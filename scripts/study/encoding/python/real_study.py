"""Encoding study on the real S3 samples: exactness, decimal exponent distribution,
block widths (FOR, delta, patched) in instrument-contiguous vs file order, Parquet
alternatives and the in-memory estimate. Writes the integer columns for the Rust
decode benchmark."""
import io, json, sys
import numpy as np
import pyarrow as pa, pyarrow.parquet as pq, pyarrow.compute as pc

ROOT = sys.argv[1]
B = 1024
HDR = 128          # bits per block header: base, width, exponent, exception count
EXC = 64 + 10      # bits per patched exception: value and position
out = {}


def bits(v):
    v = v.astype(np.float64)
    w = np.zeros(v.shape, np.int64)
    nz = v > 0
    w[nz] = np.floor(np.log2(v[nz])).astype(np.int64) + 1
    return w


def block_starts(ent):
    """Blocks of at most B rows that never straddle an instrument."""
    n = len(ent)
    if n == 0:
        return np.array([0])
    change = np.flatnonzero(ent[1:] != ent[:-1]) + 1
    starts = np.concatenate([[0], change])
    ends = np.concatenate([change, [n]])
    s = [np.arange(a, b, B) for a, b in zip(starts, ends)]
    return np.concatenate(s)


def widths(ints, starts):
    """Per-block FOR, delta and patched-FOR bits; returns totals in bytes/value and width quantiles."""
    n = len(ints)
    lens = np.diff(np.concatenate([starts, [n]]))
    mn = np.minimum.reduceat(ints, starts); mx = np.maximum.reduceat(ints, starts)
    w_for = bits((mx - mn).astype(np.float64))
    d = np.diff(ints, prepend=ints[:1]); d[starts] = 0
    zz = np.where(d >= 0, 2 * d, -2 * d - 1)
    w_delta = bits(np.maximum.reduceat(zz, starts).astype(np.float64))
    # Patched FOR: per block the width b minimising lens*b + EXC*#(values needing more than b bits).
    off = ints - np.repeat(mn, lens)
    bi = bits(off.astype(np.float64))
    best = None
    for b in range(0, 64):
        over = np.add.reduceat((bi > b).astype(np.int64), starts)
        cost = lens * b + EXC * over
        best = cost if best is None else np.minimum(best, cost)
        if b > w_for.max():
            break
    # Patched delta: same on the zig-zag deltas.
    bz = bits(zz.astype(np.float64)); bestd = None
    for b in range(0, 64):
        over = np.add.reduceat((bz > b).astype(np.int64), starts)
        cost = lens * b + EXC * over
        bestd = cost if bestd is None else np.minimum(bestd, cost)
        if b > w_delta.max():
            break
    nb = len(starts)
    return {
        "blocks": int(nb), "mean_block_len": round(float(lens.mean()), 1),
        "for_bpv": round(float((lens * w_for).sum() + nb * HDR) / 8 / n, 3),
        "delta_bpv": round(float((lens * w_delta).sum() + nb * HDR) / 8 / n, 3),
        "pfor_bpv": round(float(best.sum() + nb * HDR) / 8 / n, 3),
        "pdelta_bpv": round(float(bestd.sum() + nb * HDR) / 8 / n, 3),
        "for_w_p50_p95": [int(np.percentile(w_for, 50)), int(np.percentile(w_for, 95))],
        "delta_w_p50_p95": [int(np.percentile(w_delta, 50)), int(np.percentile(w_delta, 95))],
    }


def exponent_f32(x32, emax=9):
    x = x32.astype(np.float64); e = np.full(x.shape, emax + 1, np.int8); todo = np.ones(x.shape, bool)
    for k in range(emax + 1):
        s = 10.0 ** k; idx = np.flatnonzero(todo)
        ok = (np.round(x[idx] * s) / s).astype(np.float32) == x32[idx]
        e[idx[ok]] = k; todo[idx[ok]] = False
        if not todo.any(): break
    return e


def exponent_f64(x, emax=9):
    e = np.full(x.shape, emax + 1, np.int8); todo = np.ones(x.shape, bool)
    for k in range(emax + 1):
        s = 10.0 ** k; idx = np.flatnonzero(todo)
        ok = np.round(x[idx] * s) / s == x[idx]
        e[idx[ok]] = k; todo[idx[ok]] = False
        if not todo.any(): break
    return e


def pq_bytes(arr, **kw):
    buf = io.BytesIO(); pq.write_table(pa.table({"x": arr}), buf, compression="zstd", compression_level=3, **kw); return buf.tell()


def column(name, vals, ent_sorted_order, ent_sorted, file_order_vals, kind, actual_bytes, dump=None):
    """vals: values in instrument-contiguous order (entity, time); file_order_vals: same values in file order."""
    n = len(vals)
    if n == 0:
        return {"rows": 0}
    r = {"rows": n, "actual_parquet_bpv": round(actual_bytes / n, 3) if actual_bytes else None}
    starts = block_starts(ent_sorted)
    if kind == "int":
        ints = vals.astype(np.int64); r["range"] = [int(ints.min()), int(ints.max())]
        r["parquet_int64_delta_zstd_bpv"] = round(pq_bytes(pa.array(ints), column_encoding={"x": "DELTA_BINARY_PACKED"}, use_dictionary=False) / n, 3)
        fo = file_order_vals.astype(np.int64)
    else:
        if kind == "f32":
            x32 = vals.astype(np.float32); r["is_f32_share"] = 1.0 if vals.dtype == np.float32 else round(float(np.mean(x32.astype(np.float64) == vals)), 6)
            e = exponent_f32(x32)
        else:
            e = exponent_f64(vals)
        u, c = np.unique(e, return_counts=True); r["exponent_share"] = {int(k): round(float(v) / n, 6) for k, v in zip(u, c)}
        # Per block exponent = the largest needed by its values up to 9; values needing more are exceptions.
        eb = np.maximum.reduceat(np.minimum(e, 9).astype(np.int64), starts)
        lens = np.diff(np.concatenate([starts, [n]])); ev = np.repeat(eb, lens)
        scale = 10.0 ** ev
        ints = np.round(vals.astype(np.float64) * scale).astype(np.int64)
        back = (ints / scale)
        exact = (back.astype(np.float32) == vals.astype(np.float32)) if kind == "f32" else (back == vals)
        r["roundtrip_exact_share"] = round(float(exact.mean()), 8); r["exceptions"] = int((~exact).sum())
        r["block_exponent_p50_max"] = [int(np.median(eb)), int(eb.max())]
        # Parquet alternatives.
        r["parquet_f32_bss_zstd_bpv"] = round(pq_bytes(pa.array(vals.astype(np.float32)), use_byte_stream_split=["x"], use_dictionary=False) / n, 3)
        r["parquet_f64_plain_zstd_bpv"] = round(pq_bytes(pa.array(vals.astype(np.float64)), use_dictionary=False) / n, 3)
        emax = int(np.minimum(e, 9).max()); sc = 10.0 ** emax
        fixed = np.round(vals.astype(np.float64) * sc).astype(np.int64)
        r["parquet_decimal_e"] = emax
        r["parquet_int64_delta_zstd_bpv"] = round(pq_bytes(pa.array(fixed), column_encoding={"x": "DELTA_BINARY_PACKED"}, use_dictionary=False) / n, 3)
        fo = np.round(file_order_vals.astype(np.float64) * sc).astype(np.int64)
        # Distribution of steps in units of the block quantum (what sets the delta width).
        d = np.abs(np.diff(ints)); d = d[np.diff(ent_sorted) == 0] if len(ent_sorted) == n else d
        if len(d):
            r["abs_step_quantiles_p50_p90_p99_p999_max"] = [int(np.percentile(d, q)) for q in (50, 90, 99, 99.9)] + [int(d.max())]
    r["mem_instrument_contiguous"] = widths(ints, starts)
    r["mem_file_order"] = {k: v for k, v in widths(fo, np.arange(0, len(fo), B)).items() if k.endswith("bpv")}
    if dump is not None:
        ints.astype("<i8").tofile(dump + ".i64"); starts.astype("<i8").tofile(dump + ".starts")
        if kind != "int":
            vals.astype("<f4").tofile(dump + ".f32"); eb.astype("<i8").tofile(dump + ".exp")
    return r


def actual_sizes(path):
    md = pq.ParquetFile(path).metadata; s = {}
    for g in range(md.num_row_groups):
        rg = md.row_group(g)
        for i in range(rg.num_columns):
            c = rg.column(i); s[c.path_in_schema] = s.get(c.path_in_schema, 0) + c.total_compressed_size
    return s


# ---------------- NDLake ----------------
for year in ("1995", "2024"):
    path = f"{ROOT}/ndlake/{year}.parquet"; t = pq.read_table(path); act = actual_sizes(path)
    eq = t.filter(pc.equal(t["series_type"], "equity"))
    s = eq.sort_by([("asset_id", "ascending"), ("date", "ascending")])
    ent = s["asset_id"].to_numpy()
    res = {"rows_all": t.num_rows, "rows_equity": eq.num_rows, "assets": int(len(np.unique(ent)))}
    for col in ("close", "open", "high", "low", "turnover"):
        v = s[col].to_numpy(zero_copy_only=False); ok = ~np.isnan(v)
        fv = eq[col].to_numpy(zero_copy_only=False); fv = fv[~np.isnan(fv)]
        res[col] = column(col, v[ok], None, ent[ok], fv, "f32", act.get(col, 0) * eq.num_rows / t.num_rows,
                          dump=f"{ROOT}/bench_nd{year}_{col}" if col == "close" else None)
    v = s["volume"].to_numpy(zero_copy_only=False)
    res["volume"] = column("volume", v, None, ent, eq["volume"].to_numpy(zero_copy_only=False), "int", act.get("volume", 0) * eq.num_rows / t.num_rows,
                           dump=f"{ROOT}/bench_nd{year}_volume" if year == "2024" else None)
    out[f"ndlake_{year}"] = res
    print(f"ndlake {year} done", file=sys.stderr)

# ---------------- Databento ----------------
import glob, os
defs = {}
for f in glob.glob(f"{ROOT}/dbn/def/*.parquet"):
    d = pq.read_table(f).to_pydict()
    for iid, tick, sym in zip(d["instrument_id"], d["min_price_increment"], d["raw_symbol"]):
        defs[int(iid)] = (tick, sym)
dbn = {}
for root in ("ES", "ZN", "ZT", "CL", "GC", "6J"):
    files = sorted(glob.glob(f"{ROOT}/dbn/{root}_*.parquet")); t = pa.concat_tables([pq.read_table(f) for f in files])
    act = {}
    for f in files:
        for k, v in actual_sizes(f).items(): act[k] = act.get(k, 0) + v
    s = t.sort_by([("instrument_id", "ascending"), ("ts_event", "ascending")]); ent = s["instrument_id"].to_numpy().astype(np.int64)
    syms = s["symbol"].to_numpy(zero_copy_only=False); spread = np.array(["-" in x for x in syms])
    close = s["close"].to_numpy()
    ticks = np.array([defs.get(int(i), (np.nan, ""))[0] for i in ent])
    on_tick = np.isfinite(ticks) & (np.abs(close / np.where(ticks > 0, ticks, np.nan) - np.round(close / np.where(ticks > 0, ticks, np.nan))) < 1e-6)
    r = {"rows": t.num_rows, "instruments": int(len(np.unique(ent))), "spread_rows_share": round(float(spread.mean()), 4),
         "ticks_seen": sorted({float(x) for x in ticks[np.isfinite(ticks)]}),
         "close_on_definition_tick_share": round(float(on_tick[np.isfinite(ticks)].mean()), 6) if np.isfinite(ticks).any() else None,
         "close_is_f32_share": round(float(np.mean(close.astype(np.float32).astype(np.float64) == close)), 6)}
    r["close"] = column("close", close, None, ent, t["close"].to_numpy(), "f64", act.get("close", 0),
                        dump=f"{ROOT}/bench_dbn_{root}_close" if root in ("ES", "CL") else None)
    r["volume"] = column("volume", s["volume"].to_numpy().astype(np.int64), None, ent, t["volume"].to_numpy().astype(np.int64), "int", act.get("volume", 0))
    dbn[root] = r
    print(f"dbn {root} done", file=sys.stderr)
out["databento"] = dbn

# ---------------- ORATS ----------------
files = sorted(glob.glob(f"{ROOT}/orats/*.parquet"))
day = pq.read_table(files[0]); act = actual_sizes(files[0])
day = day.sort_by([("ticker", "ascending"), ("expirDate", "ascending"), ("strike", "ascending")])
# Within a day, the chain order: one block per (ticker, expiry).
key = pc.binary_join_element_wise(day["ticker"], pc.cast(day["expirDate"], pa.string()), "|")
ent = np.unique(key.to_numpy(zero_copy_only=False), return_inverse=True)[1]
orats = {"rows_per_day": day.num_rows, "chains": int(ent.max() + 1)}
for col in ("stkPx", "strike", "yte", "cBidPx", "cAskPx", "cValue", "cMidIv", "smoothSmvVol", "delta", "gamma", "vega", "theta", "iRate", "cVolu", "cOi"):
    v = day[col].to_numpy(zero_copy_only=False); kind = "int" if v.dtype.kind in "iu" else "f32"
    ok = np.ones(len(v), bool) if kind == "int" else ~np.isnan(v)
    orats[col] = column(col, v[ok], None, ent[ok], v[ok], kind, act.get(col, 0) * ok.mean(),
                        dump=f"{ROOT}/bench_orats_{col}" if col in ("cBidPx", "cMidIv", "cOi") else None)
    print(f"orats {col} done", file=sys.stderr)
# Across days: the same contract's history (3 days here), to see how quotes and OI evolve.
three = pa.concat_tables([pq.read_table(f, columns=["ticker", "expirDate", "strike", "trade_date", "cBidPx", "cOi", "cMidIv"]) for f in files])
three = three.sort_by([("ticker", "ascending"), ("expirDate", "ascending"), ("strike", "ascending"), ("trade_date", "ascending")])
k3 = pc.binary_join_element_wise(three["ticker"], pc.cast(three["expirDate"], pa.string()), pc.cast(three["strike"], pa.string()), "|")
e3 = np.unique(k3.to_numpy(zero_copy_only=False), return_inverse=True)[1]
for col in ("cBidPx", "cOi", "cMidIv"):
    v = three[col].to_numpy(zero_copy_only=False); kind = "int" if v.dtype.kind in "iu" else "f32"
    ok = np.ones(len(v), bool) if kind == "int" else ~np.isnan(v)
    orats[f"{col}_contract_history_3d"] = column(col, v[ok], None, e3[ok], v[ok], kind, None)["mem_instrument_contiguous"]
out["orats"] = orats

json.dump(out, open(f"{ROOT}/study.json", "w"), indent=1)
print("written", file=sys.stderr)
