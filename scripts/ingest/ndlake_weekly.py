#!/usr/bin/env python3
"""NDLake daily bars -> the weekly Parquet inputs of `abt bundle build --from DIR
--env equities_1w` (corpus/env/equities_1w.dsl), mirroring the data layer of the
MW14 research book (stageanalyst strategy/ivmom/data.py, `load_daily`, `to_weekly`,
`add_membership_and_universe`, `attach_gate_index`) so that abt reads the same
weekly facts. Only raw facts are written; every signal is computed by the strategy.

  python scripts/ingest/ndlake_weekly.py --lake LAKE --artifacts DIR --out OUT

LAKE holds curated/bars_by_date, registry/corporate_actions and
indexes/index_membership-*.parquet; DIR holds spxtr_daily.parquet and
vix_weekly.parquet (the book's own gate and volatility inputs).

The week is the Monday-truncated calendar week. Every relation is keyed at the
market's week end: the last trading day of that week over all equities (an asset
that last traded on Thursday is still keyed at Friday, as the book keys rows by
week, not by the asset's own date). Per asset and week:

  close     the asset's last raw close of the week (the $3 floor reads it)
  trclose   the total-return index: the running product of the weekly factor,
            itself the product of the daily (close + dividend) x split / previous
            close; the executor fills and marks on it (--actions in-prices)
  split     the product of the week's split ratios (new shares per old), when not 1
  dvol      the week's mean daily raw close x volume
  universe  every asset-week with a bar (members and not)
  member    `SP1500` when the asset's own week end lies in a spell of the S&P
            500, 400 or 600 (member_from <= week_end <= member_to, inclusive,
            an open spell ending 2100-01-01)
  delisted  `ended` at the first market week after an asset's last bar, when that
            is before the data's last week
  series    SPXTR (the $SPXTR total-return index, built in float32 as the book
            does), VIX and VIX_MA4 (from vix_weekly, forward-filled onto the
            gate's weeks)

Security ids are the asset ids zero-padded to 10 digits, so that the identifier
order (`A asc`) is the book's ascending asset_id. SPY and QQQ are left out (never
in the book's universe).
"""
import argparse
import json
import sys
import time
from pathlib import Path

import duckdb
import numpy as np
import pandas as pd


def log(msg: str) -> None:
    print(msg, file=sys.stderr, flush=True)


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--lake", required=True, type=Path)
    ap.add_argument("--artifacts", required=True, type=Path)
    ap.add_argument("--out", required=True, type=Path)
    ap.add_argument("--start", default="1990-01-01")
    ap.add_argument("--warmup-weeks", type=int, default=0,
                    help="drop an asset's rows earlier than this many of its weeks before its first membership (0 keeps all)")
    args = ap.parse_args()
    started = time.time()
    out = args.out
    out.mkdir(parents=True, exist_ok=True)
    con = duckdb.connect()
    lake = args.lake
    report: dict = {"inputs": {"lake": str(lake), "artifacts": str(args.artifacts)}}

    # Daily bars and splits (data.py load_daily).
    con.execute(f"""
        CREATE TABLE d AS
        SELECT b.asset_id, b.ticker, b.date, b.close, b.volume, b.div_cash,
               coalesce(s.split, 1.0) AS split
        FROM read_parquet('{lake}/curated/bars_by_date/year=*/*.parquet', hive_partitioning = 1) b
        LEFT JOIN (
            SELECT asset_id, ex_date AS date, any_value(split_ratio) AS split
            FROM read_parquet('{lake}/registry/corporate_actions/*.parquet')
            WHERE action_type = 'SPLIT' GROUP BY 1, 2
        ) s USING (asset_id, date)
        WHERE b.date >= DATE '{args.start}' AND b.series_type = 'equity' AND b.ticker NOT IN ('SPY', 'QQQ')
    """)
    report["daily_rows"] = con.execute("SELECT count(*) FROM d").fetchone()[0]
    log(f"daily rows {report['daily_rows']}")
    # The daily total-return factor over each asset's own previous row.
    con.execute("""
        CREATE TABLE dt AS
        SELECT *, date_trunc('week', date)::DATE AS wk,
               close * volume AS dvol,
               (close + coalesce(div_cash, 0.0)) * split / lag(close) OVER (PARTITION BY asset_id ORDER BY date) AS trf
        FROM d
    """)
    # Weekly aggregates (data.py to_weekly); the product skips nulls, like polars.
    con.execute("""
        CREATE TABLE w AS
        SELECT asset_id, wk, max(date) AS asset_week_end,
               arg_max(close, date) AS raw_close,
               product(trf) AS wk_trf,
               avg(dvol) AS wk_dvol,
               product(split) AS wk_split
        FROM dt GROUP BY 1, 2
    """)
    # Market week ends.
    con.execute("CREATE TABLE cal AS SELECT wk, max(date) AS week_end FROM dt GROUP BY 1")
    # Membership at the asset's own week end.
    con.execute(f"""
        CREATE TABLE mem AS SELECT asset_id, member_from, coalesce(member_to, DATE '2100-01-01') AS member_to
        FROM read_parquet('{lake}/indexes/index_membership-*.parquet')
    """)
    con.execute("""
        CREATE TABLE wm AS
        SELECT w.*, c.week_end,
               EXISTS (SELECT 1 FROM mem m WHERE m.asset_id = w.asset_id AND w.asset_week_end BETWEEN m.member_from AND m.member_to) AS in_sp1500
        FROM w JOIN cal c USING (wk)
    """)
    if args.warmup_weeks > 0:
        con.execute(f"""
            CREATE TABLE first_member AS
            SELECT asset_id, min(wk) AS fwk FROM wm WHERE in_sp1500 GROUP BY 1;
            DELETE FROM wm WHERE asset_id NOT IN (SELECT asset_id FROM first_member);
            CREATE TABLE ranked AS
            SELECT wm.*, row_number() OVER (PARTITION BY asset_id ORDER BY wk) AS rn FROM wm;
            CREATE TABLE cut AS
            SELECT r.asset_id, max(r.rn) FILTER (WHERE r.wk <= f.fwk) - {args.warmup_weeks} AS keep_from
            FROM ranked r JOIN first_member f USING (asset_id) GROUP BY 1;
            DELETE FROM wm WHERE (asset_id, wk) IN (
                SELECT r.asset_id, r.wk FROM ranked r JOIN cut USING (asset_id) WHERE r.rn < cut.keep_from);
        """)
    df = con.execute("SELECT * FROM wm ORDER BY asset_id, wk").df()
    # The total-return index: the running product of the weekly factor (null -> 1).
    df["tri"] = df["wk_trf"].fillna(1.0).groupby(df["asset_id"]).cumprod()
    df["A"] = df["asset_id"].map(lambda a: f"{int(a):010d}")
    df["T"] = pd.to_datetime(df["week_end"])
    report["weekly_rows"] = int(len(df))
    report["assets"] = int(df["asset_id"].nunique())
    log(f"weekly rows {len(df)} over {df['asset_id'].nunique()} assets")

    def write(name: str, frame: pd.DataFrame) -> None:
        frame.to_parquet(out / f"{name}.parquet", index=False)
        report.setdefault("written", {})[name] = int(len(frame))
        log(f"wrote {name}: {len(frame)}")

    write("universe", df[["A", "T"]])
    write("close", df[["A", "T"]].assign(P=df["raw_close"]))
    write("trclose", df[["A", "T"]].assign(P=df["tri"]))
    write("dvol", df[["A", "T"]].assign(D=df["wk_dvol"]))
    sp = df[df["wk_split"].notna() & (df["wk_split"] != 1.0)]
    write("split", sp[["A", "T"]].assign(Factor=sp["wk_split"]))
    mem = df[df["in_sp1500"]]
    write("member", mem[["A", "T"]].assign(Idx="SP1500"))
    # Securities: one id per asset, its last ticker, over its rows.
    last = con.execute("SELECT asset_id, arg_max(ticker, date) AS ticker FROM d GROUP BY 1").df().set_index("asset_id")["ticker"]
    spans = df.groupby("asset_id")["T"].agg(["min", "max"])
    sec = pd.DataFrame({
        "id": [f"{int(a):010d}" for a in spans.index],
        # Tickers are reused across assets over time: the id is the ticker, the
        # vendor's symbol is kept as a label in the report only.
        "ticker": [f"{int(a):010d}" for a in spans.index],
        "from": spans["min"].values,
        "to": pd.NaT,
    })
    write("securities", sec)
    report["tickers"] = {f"{int(a):010d}": t for a, t in last.items() if a in spans.index}
    # Delistings: the first market week after an asset's last row, before the data's end.
    weeks = con.execute("SELECT week_end FROM cal ORDER BY wk").df()["week_end"]
    weeks = pd.to_datetime(weeks).reset_index(drop=True)
    last_row = spans["max"]
    nxt = np.searchsorted(weeks.values, last_row.values, side="right")
    ok = nxt < len(weeks)
    dl = pd.DataFrame({"A": [f"{int(a):010d}" for a in last_row.index[ok]], "T": weeks.values[nxt[ok]], "Reason": "ended"})
    write("delisted", dl)

    # Gate: $SPXTR weekly total-return index in float32 (data.py attach_gate_index).
    sx = pd.read_parquet(args.artifacts / "spxtr_daily.parquet")
    sx = sx[sx["symbol"] == "$SPXTR"].copy()
    sx["date"] = pd.to_datetime(sx["date"]).dt.normalize()
    sx = sx.sort_values("date")
    close32 = sx["close"].astype(np.float32).to_numpy()
    trf32 = np.full(len(close32), np.nan, dtype=np.float32)
    trf32[1:] = close32[1:] / close32[:-1]
    sx["trf"] = trf32
    sx["wk"] = sx["date"].dt.to_period("W-SUN").dt.start_time
    g = sx.groupby("wk").agg(week_end=("date", "max"))
    prods = []
    for wk, grp in sx.groupby("wk"):
        p = np.float32(1.0)
        for v in grp["trf"].to_numpy():
            if not np.isnan(v):
                p = np.float32(p * v)
        prods.append(p)
    g["wk_trf"] = np.array(prods, dtype=np.float32)
    tri32 = np.cumprod(g["wk_trf"].to_numpy(dtype=np.float32), dtype=np.float32)
    g["tri"] = tri32.astype(np.float64)
    # Key the gate's weeks at the equities' week end where there is one.
    calw = con.execute("SELECT wk, week_end FROM cal").df()
    calw["wk"] = pd.to_datetime(calw["wk"])
    g = g.reset_index().merge(calw.rename(columns={"week_end": "mkt_end"}), on="wk", how="left")
    g["T"] = pd.to_datetime(g["mkt_end"]).fillna(g["week_end"])
    vix = pd.read_parquet(args.artifacts / "vix_weekly.parquet")[["wk", "vix", "vix_ma4"]]
    vix["wk"] = pd.to_datetime(vix["wk"])
    g = g.merge(vix, on="wk", how="left").sort_values("wk")
    g[["vix", "vix_ma4"]] = g[["vix", "vix_ma4"]].ffill()
    series = [g[["T"]].assign(Name="SPXTR", V=g["tri"])]
    for col, name in (("vix", "VIX"), ("vix_ma4", "VIX_MA4")):
        part = g[g[col].notna()]
        series.append(part[["T"]].assign(Name=name, V=part[col]))
    write("series", pd.concat(series, ignore_index=True))
    report["gate_weeks"] = int(len(g))
    report["seconds"] = round(time.time() - started, 1)
    (out / "ingest-report.json").write_text(json.dumps(report, indent=1, default=str))
    log(f"done in {report['seconds']} s")
    return 0


if __name__ == "__main__":
    sys.exit(main())
