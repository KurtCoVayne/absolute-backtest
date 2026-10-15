"""Distribution-aware encoding study for the store's numeric columns.

For every numeric column of the given Parquet files, measured per value and per block:
  - f32 exactness (is the stored value an f32?),
  - the smallest decimal exponent e in 0..9 at which the value round-trips bit-exactly
    (f32(i / 10^e) == x), i.e. how many decimals the vendor's number really carries,
  - per block of B values of one entity in time order (and, for comparison, in the
    file's own order): the FOR width, the delta width, and the patched-FOR width b*
    = argmin_b [B*b + C*#{v - base >= 2^b}] with C the cost of an exception,
  - bytes per value for: f64, f32 BYTE_STREAM_SPLIT+zstd (Parquet), INT64
    DELTA_BINARY_PACKED+zstd in entity-major order (Parquet DECIMAL), and the
    in-memory ALP+PFOR estimate,
  - exactness of the round trip over every value (must be 100 %, else the value is an exception).

Usage: python3 -I encoding_study.py --entity asset_id --time date FILE.parquet [...]
"""
import argparse, io, json, sys
import numpy as np
import pyarrow as pa, pyarrow.parquet as pq

EXC_BITS = 64 + 10          # a patched exception: the value and its position in the block
HDR_BITS = 128              # per block: base (64), width (8), exponent (8), exception count, padding


def min_exponent(x32: np.ndarray, emax: int = 9) -> np.ndarray:
    """Smallest e in 0..emax with f32(round(x*10^e)/10^e) == x, else emax+1 (an exception)."""
    x = x32.astype(np.float64)
    out = np.full(x.shape, emax + 1, dtype=np.int8)
    todo = np.ones(x.shape, dtype=bool)
    for e in range(emax + 1):
        s = 10.0 ** e
        i = np.round(x[todo] * s)
        ok = (i / s).astype(np.float32) == x32[todo]
        idx = np.flatnonzero(todo)[ok]
        out[idx] = e
        todo[idx] = False
        if not todo.any():
            break
    return out


def bits(v: np.ndarray) -> np.ndarray:
    v = np.asarray(v, dtype=np.uint64)
    out = np.zeros(v.shape, dtype=np.int64)
    nz = v > 0
    out[nz] = np.floor(np.log2(v[nz].astype(np.float64))).astype(np.int64) + 1
    return out


def block_widths(ints: np.ndarray, B: int, runs=None):
    """Per block: FOR width, delta width (zig-zag), patched-FOR optimal width and its bits.
    With `runs` (entity run lengths), blocks never straddle two entities."""
    res = {"for": [], "delta": [], "pfor": [], "pfor_bits": [], "for_bits": [], "delta_bits": []}
    starts = [0] if runs is None else list(np.cumsum([0] + list(runs[:-1])))
    ends = [len(ints)] if runs is None else list(np.cumsum(runs))
    spans = [(k, min(k + B, e)) for s0, e in zip(starts, ends) for k in range(s0, e, B)]
    for k0, k1 in spans:
        blk = ints[k0:k1]
        n = len(blk)
        if n == 0:
            continue
        off = (blk - blk.min()).astype(np.uint64)
        wb = bits(off)
        w_for = int(wb.max()) if n else 0
        d = np.diff(blk, prepend=blk[0])
        zz = ((d << 1) ^ (d >> 63)).astype(np.uint64)
        w_delta = int(bits(zz).max())
        hist = np.bincount(wb, minlength=65)
        above = n - np.cumsum(hist)               # values needing more than b bits
        costs = [n * b + EXC_BITS * int(above[b]) for b in range(0, 65)]
        b_star = int(np.argmin(costs))
        res["for"].append(w_for); res["delta"].append(w_delta); res["pfor"].append(b_star)
        res["for_bits"].append(n * w_for + HDR_BITS)
        res["delta_bits"].append(n * w_delta + HDR_BITS)
        res["pfor_bits"].append(costs[b_star] + HDR_BITS)
    return res


def parquet_bytes(arr: pa.Array, **kw) -> int:
    buf = io.BytesIO()
    pq.write_table(pa.table({"x": arr}), buf, compression="zstd", compression_level=3, **kw)
    return buf.tell()


def study(table: pa.Table, entity: str, time: str, B: int):
    t = table.sort_by([(entity, "ascending"), (time, "ascending")])
    ent = t[entity].to_numpy(zero_copy_only=False)
    change = np.flatnonzero(ent[1:] != ent[:-1]) + 1
    runs_all = np.diff(np.concatenate([[0], change, [len(ent)]]))
    report = {}
    for name in t.column_names:
        if name in (entity, time):
            continue
        col = t[name]
        if not (pa.types.is_floating(col.type) or pa.types.is_integer(col.type)):
            continue
        a = col.to_numpy(zero_copy_only=False)
        mask = ~np.isnan(a) if a.dtype.kind == "f" else np.ones(len(a), bool)
        a = a[mask]
        n = len(a)
        if n == 0:
            continue
        r = {"rows": int(n), "type": str(col.type)}
        if a.dtype.kind == "f":
            x32 = a.astype(np.float32)
            r["is_f32"] = float(np.mean(x32.astype(np.float64) == a))
            e = min_exponent(x32)
            r["exponent_share"] = {int(k): round(float(v), 6) for k, v in zip(*np.unique(e, return_counts=True)) for v in [v / n]}
            # Per block: the exponent is the block's largest needed (values needing more are exceptions).
            ints = np.empty(n, dtype=np.int64)
            exact = 0
            for k in range(0, n, B):
                eb = int(min(9, e[k:k + B].max()))
                s = 10.0 ** eb
                iv = np.round(x32[k:k + B].astype(np.float64) * s)
                exact += int(np.sum((iv / s).astype(np.float32) == x32[k:k + B]))
                ints[k:k + B] = iv.astype(np.int64)
            r["roundtrip_exact"] = exact / n
            r["bytes_f64"] = 8.0
            r["bytes_parquet_f32_bss_zstd"] = parquet_bytes(pa.array(x32), use_byte_stream_split=["x"], use_dictionary=False) / n
        else:
            ints = a.astype(np.int64)
            r["bytes_i64"] = 8.0
        r["bytes_parquet_int64_delta_zstd"] = parquet_bytes(pa.array(ints), column_encoding={"x": "DELTA_BINARY_PACKED"}, use_dictionary=False) / n
        runs = runs_all if len(ints) == len(ent) else None
        w = block_widths(ints, B, runs)
        for k in ("for", "delta", "pfor"):
            r[f"width_{k}_p50"] = float(np.median(w[k])); r[f"width_{k}_p95"] = float(np.percentile(w[k], 95))
        for k in ("for_bits", "delta_bits", "pfor_bits"):
            r[f"bytes_{k[:-5]}_mem"] = float(np.sum(w[k]) / 8 / n)
        # The same column in the file's own (time-major) order, for the layout decision.
        own = table[name].to_numpy(zero_copy_only=False)
        own = own[~np.isnan(own)] if own.dtype.kind == "f" else own
        if own.dtype.kind == "f":
            own = np.round(own.astype(np.float32).astype(np.float64) * 10.0 ** int(min(9, np.percentile(e, 99.9)))).astype(np.int64)
        wo = block_widths(own.astype(np.int64), B)
        r["bytes_pfor_mem_time_major"] = float(np.sum(wo["pfor_bits"]) / 8 / len(own))
        report[name] = r
    return report


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--entity", required=True); ap.add_argument("--time", required=True)
    ap.add_argument("--block", type=int, default=1024); ap.add_argument("files", nargs="+")
    a = ap.parse_args()
    table = pa.concat_tables([pq.read_table(f) for f in a.files], promote_options="default")
    json.dump(study(table, a.entity, a.time, a.block), sys.stdout, indent=1)


if __name__ == "__main__":
    main()
