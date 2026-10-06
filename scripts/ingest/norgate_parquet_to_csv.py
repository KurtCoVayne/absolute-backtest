#!/usr/bin/env python3
"""Convert the NDLake Norgate Parquet lake into the CSV layout of
`src/ingest.rs::norgate_daily` (`abt bundle build --from-norgate DIR`).

Lake layout (s3://ndlake-094828274452 or a local mirror such as ~/ndlake):

  curated/bars_by_date/year=YYYY/data.parquet   RAW (as traded) OHLCV per asset_id
  registry/security_master/data.parquet         SCD-2; status, delisting_date
  registry/corporate_actions/data.parquet       SPLIT (split_ratio, new per old) and DIV
  indexes/index_membership.parquet              half-open [member_from, member_to)
  classification/classification_full/...        GICS/TRBC, ONE snapshot (not point in time)

Output (all dates YYYY-MM-DD; the adapter's `to` columns are inclusive):

  prices.csv          symbol,date,open,high,low,close,volume      as traded
  symbols.csv         id,symbol,from,to                           id = Norgate asset_id
  splits.csv          symbol,ex_date,factor                       new shares per old
  dividends.csv       symbol,announce_date,ex_date,pay_date,amount  announce = pay = ex
  delistings.csv      symbol,date,reason                          date = first bar after the last print
  membership.csv      symbol,index,from,to
  classification.csv  symbol,scheme,code,from,to                  from = the snapshot date
  ingest-report.json  rows in/out, drops by reason, ranges, histograms

The universe is point in time: every equity that was a member of `--index`
at any time in [--from, --to], never the current list. All heavy work runs
inside DuckDB and is streamed to CSV with COPY; nothing large is loaded into
Python.

Decisions that the data forces (also written to the report):

- Delisting reasons do not exist in Norgate. The reason is inferred from the
  last prints: a ticker ending in Q (bankruptcy convention) or a last close
  under $1 -> bankruptcy; a fall of more than 50% over the last 60 bars ->
  other; anything else -> acquisition. Under the engine's default haircuts
  that is bankruptcy 1, other 1, acquisition 0. This is a heuristic, not
  data; override per run with `abt run --haircut REASON=X`.
- Dividends have no announcement or pay date in Norgate: announce = pay =
  ex-date. Announce = ex is conservative (never earlier than the truth); pay
  = ex credits cash a few weeks early (slightly optimistic on cash).
- Classification is one snapshot; it is emitted only from its snapshot date
  onward, so a strategy cannot use 2026 sectors in 2014 (look-ahead).
- Tickers are Norgate's final ticker per bar. When two asset_ids would carry
  the same ticker over overlapping dates, the delisted one is renamed to its
  Norgate source symbol (e.g. AGN-201503).
"""
from __future__ import annotations

import argparse
import json
import sys
import time
from pathlib import Path

import duckdb


def log(msg: str) -> None:
    print(msg, file=sys.stderr, flush=True)


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--lake", required=True, help="lake root: a local directory or s3://bucket")
    ap.add_argument("--out", required=True, type=Path)
    ap.add_argument("--index", default="S&P 500", help="Norgate index_name for the point-in-time universe")
    ap.add_argument("--label", default="SPX", help="the membership label the strategies use")
    ap.add_argument("--from", dest="date_from", default="2014-01-01")
    ap.add_argument("--to", dest="date_to", default=None, help="last date (default: the data's last bar)")
    ap.add_argument("--limit", type=int, default=None, help="keep only N assets (by asset_id), for smoke runs")
    ap.add_argument("--scheme", default="GICS", help="classification scheme to emit (level 1 codes)")
    ap.add_argument("--exceptions", help="a reviewed exceptions file (test,symbol,date,reason) to copy into the export")
    args = ap.parse_args()

    started = time.time()
    out: Path = args.out
    out.mkdir(parents=True, exist_ok=True)
    lake = args.lake.rstrip("/")
    con = duckdb.connect()
    con.execute("SET preserve_insertion_order = false")
    if lake.startswith("s3://"):
        con.execute("INSTALL httpfs; LOAD httpfs; CREATE SECRET (TYPE s3, PROVIDER credential_chain)")

    def p(rel: str) -> str:
        return f"'{lake}/{rel}'"

    q = lambda sql: con.execute(sql).fetchall()
    one = lambda sql: con.execute(sql).fetchone()[0]
    report: dict = {"lake": lake, "index": args.index, "label": args.label, "from": args.date_from, "drops": {}, "decisions": []}

    # 1. The point-in-time universe: every member of the index in the window.
    to_clause = f"AND member_from <= DATE '{args.date_to}'" if args.date_to else ""
    con.execute(f"""
        CREATE TEMP TABLE members AS
        SELECT asset_id, member_from, member_to FROM read_parquet({p('indexes/index_membership.parquet')})
        WHERE index_name = ? AND (member_to IS NULL OR member_to > DATE '{args.date_from}') {to_clause}
    """, [args.index])
    con.execute("CREATE TEMP TABLE ids AS SELECT DISTINCT asset_id FROM members ORDER BY asset_id" + (f" LIMIT {args.limit}" if args.limit else ""))
    report["universe_assets"] = one("SELECT count(*) FROM ids")
    log(f"universe: {report['universe_assets']} assets ever in `{args.index}` since {args.date_from}")

    # 2. Bars for those assets, equities only, in the window.
    date_to = f"AND date <= DATE '{args.date_to}'" if args.date_to else ""
    con.execute(f"""
        CREATE TEMP TABLE raw AS
        SELECT b.asset_id, b.ticker, b.source_symbol, b.date, b.open, b.high, b.low, b.close, b.volume, b.ingest_ts
        FROM read_parquet({p('curated/bars_by_date/*/data.parquet')}, hive_partitioning = true) b
        JOIN ids USING (asset_id)
        WHERE b.series_type = 'equity' AND b.date >= DATE '{args.date_from}' {date_to}
          AND b.year >= {int(args.date_from[:4])}
    """)
    rows_in = one("SELECT count(*) FROM raw")
    report["rows_in"] = rows_in
    # Duplicates: keep the latest ingest of an (asset, date).
    con.execute("""
        CREATE TEMP TABLE dedup AS
        SELECT * EXCLUDE (rn) FROM (
          SELECT *, row_number() OVER (PARTITION BY asset_id, date ORDER BY ingest_ts DESC) rn FROM raw) WHERE rn = 1
    """)
    report["drops"]["duplicate_asset_date"] = rows_in - one("SELECT count(*) FROM dedup")
    # Padded rows: no close, or a non-positive close.
    report["drops"]["missing_or_nonpositive_close"] = one("SELECT count(*) FROM dedup WHERE close IS NULL OR isnan(close) OR close <= 0")
    con.execute("DELETE FROM dedup WHERE close IS NULL OR isnan(close) OR close <= 0")
    # A missing or non-positive open/high/low is filled from the close (counted).
    report["filled"] = {
        c: one(f"SELECT count(*) FROM dedup WHERE {c} IS NULL OR isnan({c}) OR {c} <= 0") for c in ("open", "high", "low")
    }
    report["filled"]["volume_null_to_zero"] = one("SELECT count(*) FROM dedup WHERE volume IS NULL")
    con.execute("""
        CREATE TEMP TABLE bars AS
        SELECT asset_id, ticker, source_symbol, date,
               CASE WHEN open IS NULL OR isnan(open) OR open <= 0 THEN close ELSE open END AS open,
               CASE WHEN high IS NULL OR isnan(high) OR high <= 0 THEN close ELSE high END AS high,
               CASE WHEN low  IS NULL OR isnan(low)  OR low  <= 0 THEN close ELSE low  END AS low,
               close, coalesce(volume, 0) AS volume
        FROM dedup
    """)
    con.execute("DROP TABLE raw; DROP TABLE dedup")
    # Fields must carry no comma or quote (the adapter's reader has no quoting).
    bad = one("SELECT count(*) FROM bars WHERE ticker LIKE '%,%' OR ticker LIKE '%\"%' OR source_symbol LIKE '%,%'")
    if bad:
        log(f"error: {bad} rows carry a comma or quote in a ticker; refusing")
        return 2
    report["rows_out"] = one("SELECT count(*) FROM bars")
    first_day, last_day = con.execute("SELECT min(date), max(date) FROM bars").fetchone()
    report["date_range"] = [str(first_day), str(last_day)]
    con.execute("CREATE TEMP TABLE days AS SELECT DISTINCT date FROM bars ORDER BY date")
    con.execute("CREATE TEMP TABLE nextday AS SELECT date, lead(date) OVER (ORDER BY date) AS next, lag(date) OVER (ORDER BY date) AS prev FROM days")
    log(f"bars: {rows_in} in, {report['rows_out']} out, {first_day} .. {last_day}")

    # 3. Symbol history: runs of one ticker per asset (gaps and islands), each
    # run covering from its first bar to the bar before the next run.
    con.execute("""
        CREATE TEMP TABLE runs AS
        WITH t AS (
          SELECT asset_id, ticker, source_symbol, date,
                 row_number() OVER (PARTITION BY asset_id ORDER BY date)
                 - row_number() OVER (PARTITION BY asset_id, ticker ORDER BY date) AS grp
          FROM bars)
        SELECT asset_id, ticker, any_value(source_symbol) AS source_symbol, min(date) AS d0, max(date) AS d1
        FROM t GROUP BY asset_id, ticker, grp
    """)
    # The security master: delisted status and the explicit delisting date.
    con.execute(f"""
        CREATE TEMP TABLE sm AS
        SELECT asset_id, any_value(status) AS status, max(delisting_date) AS delisting_date
        FROM read_parquet({p('registry/security_master/data.parquet')}) WHERE is_current GROUP BY asset_id
    """)
    con.execute("""
        CREATE TEMP TABLE asset_span AS
        SELECT asset_id, min(date) AS first_bar, max(date) AS last_bar FROM bars GROUP BY asset_id
    """)
    # Delisted: the last bar is before the data's last day. The delisting
    # date is the first trading day after the last print.
    con.execute(f"""
        CREATE TEMP TABLE gone AS
        SELECT s.asset_id, s.last_bar, n.next AS delist_date, sm.status, sm.delisting_date
        FROM asset_span s JOIN nextday n ON n.date = s.last_bar LEFT JOIN sm USING (asset_id)
        WHERE s.last_bar < DATE '{last_day}'
    """)
    # Interval ends: next run's start minus one bar; the last run extends to
    # the delisting date for a delisted asset, else stays open.
    con.execute(f"""
        CREATE TEMP TABLE intervals AS
        WITH r AS (SELECT *, lead(d0) OVER (PARTITION BY asset_id ORDER BY d0) AS next_d0 FROM runs)
        SELECT r.asset_id, r.ticker, r.source_symbol, r.d0 AS "from",
               CASE WHEN r.next_d0 IS NOT NULL THEN n.prev
                    WHEN g.delist_date IS NOT NULL THEN g.delist_date
                    ELSE NULL END AS "to"
        FROM r LEFT JOIN nextday n ON n.date = r.next_d0 LEFT JOIN gone g USING (asset_id)
    """)
    # Ticker collisions between assets on overlapping dates: rename the
    # delisted (or earlier-ending) side to its source symbol.
    collisions = q(f"""
        SELECT DISTINCT CASE WHEN coalesce(a."to", DATE '9999-12-31') <= coalesce(b."to", DATE '9999-12-31') THEN a.asset_id ELSE b.asset_id END,
                        a.ticker
        FROM intervals a JOIN intervals b ON a.ticker = b.ticker AND a.asset_id < b.asset_id
         AND a."from" <= coalesce(b."to", DATE '9999-12-31') AND b."from" <= coalesce(a."to", DATE '9999-12-31')
    """)
    report["ticker_collisions_renamed"] = len(collisions)
    if collisions:
        con.execute("CREATE TEMP TABLE renamed (asset_id BIGINT, ticker VARCHAR)")
        con.executemany("INSERT INTO renamed VALUES (?, ?)", collisions)
        con.execute("""
            UPDATE intervals SET ticker = CASE WHEN source_symbol <> ticker THEN source_symbol ELSE ticker || '-' || asset_id END
            WHERE (asset_id, ticker) IN (SELECT asset_id, ticker FROM renamed)
        """)
        left = one("""SELECT count(*) FROM intervals a JOIN intervals b ON a.ticker = b.ticker AND a.asset_id < b.asset_id
             AND a."from" <= coalesce(b."to", DATE '9999-12-31') AND b."from" <= coalesce(a."to", DATE '9999-12-31')""")
        if left:
            log(f"error: {left} ticker collisions remain after renaming")
            return 2
    # Every bar row gets the (possibly renamed) symbol of its interval.
    con.execute("""
        CREATE TEMP TABLE sym_bars AS
        SELECT i.ticker AS symbol, b.* EXCLUDE (ticker, source_symbol)
        FROM bars b JOIN intervals i ON i.asset_id = b.asset_id AND b.date >= i."from" AND (i."to" IS NULL OR b.date <= i."to")
    """)
    assert one("SELECT count(*) FROM sym_bars") == report["rows_out"], "every bar maps to one symbol interval"
    report["symbols"] = one("SELECT count(DISTINCT asset_id) FROM intervals")
    report["symbol_intervals"] = one("SELECT count(*) FROM intervals")

    def copy(sql: str, name: str) -> int:
        path = out / name
        con.execute(f"COPY ({sql}) TO '{path}' (HEADER, DELIMITER ',', QUOTE '', ESCAPE '', DATEFORMAT '%Y-%m-%d')")
        n = one(f"SELECT count(*) FROM ({sql})")
        log(f"wrote {name}: {n} rows")
        return n

    report["written"] = {}
    report["written"]["prices.csv"] = copy(
        # The lake stores float32-derived doubles; 6 decimals keeps sub-cent prints positive.
        """SELECT symbol, date, round(open, 6) AS open, round(high, 6) AS high, round(low, 6) AS low,
                  round(close, 6) AS close, volume FROM sym_bars ORDER BY date, symbol""", "prices.csv")
    report["written"]["symbols.csv"] = copy(
        """SELECT asset_id AS id, ticker AS symbol, "from", "to" FROM intervals ORDER BY asset_id, "from" """, "symbols.csv")

    # Helper: the symbol carrying an asset on a date.
    sym_at = """(SELECT i.ticker FROM intervals i WHERE i.asset_id = {a} AND {d} >= i."from" AND (i."to" IS NULL OR {d} <= i."to"))"""

    # 4. Corporate actions.
    con.execute(f"""
        CREATE TEMP TABLE ca AS
        SELECT c.* FROM read_parquet({p('registry/corporate_actions/data.parquet')}) c JOIN ids USING (asset_id)
        WHERE c.ex_date >= DATE '{args.date_from}' AND c.ex_date <= DATE '{last_day}'
    """)
    # Splits must land on a bar of the asset (the reconciliation test needs the close).
    split_all = one("SELECT count(*) FROM ca WHERE action_type = 'SPLIT'")
    con.execute("""
        CREATE TEMP TABLE splits AS
        SELECT b.symbol, c.ex_date, c.split_ratio AS factor FROM ca c
        JOIN sym_bars b ON b.asset_id = c.asset_id AND b.date = c.ex_date
        WHERE c.action_type = 'SPLIT' AND c.split_ratio > 0 AND c.split_ratio <> 1
    """)
    n_splits = copy("SELECT symbol, ex_date, factor FROM splits ORDER BY ex_date, symbol", "splits.csv")
    report["written"]["splits.csv"] = n_splits
    report["drops"]["split_not_on_a_bar_or_unit"] = split_all - n_splits
    div_all = one("SELECT count(*) FROM ca WHERE action_type = 'DIV'")
    # Norgate quotes a dividend per share before a same-day split; the
    # executor pays it on the shares after the split, so rescale it.
    con.execute(f"""
        CREATE TEMP TABLE divs AS
        SELECT {sym_at.format(a='c.asset_id', d='c.ex_date')} AS symbol, c.ex_date,
               c.div_cash / coalesce(s.split_ratio, 1.0) AS amount, s.split_ratio IS NOT NULL AS rescaled
        FROM ca c LEFT JOIN (SELECT asset_id, ex_date, any_value(split_ratio) AS split_ratio FROM ca
                             WHERE action_type = 'SPLIT' AND split_ratio > 0 AND split_ratio <> 1 GROUP BY ALL) s
               ON s.asset_id = c.asset_id AND s.ex_date = c.ex_date
        WHERE c.action_type = 'DIV' AND c.div_cash > 0
    """)
    report["dividends_rescaled_for_same_day_split"] = one("SELECT count(*) FROM divs WHERE rescaled")
    report["drops"]["dividend_outside_symbol_span_or_nonpositive"] = div_all - one("SELECT count(*) FROM divs WHERE symbol IS NOT NULL")
    report["written"]["dividends.csv"] = copy(
        "SELECT symbol, ex_date AS announce_date, ex_date, ex_date AS pay_date, round(amount, 6) AS amount FROM divs WHERE symbol IS NOT NULL ORDER BY ex_date, symbol",
        "dividends.csv")

    # 5. Delistings with an inferred reason (see the module docstring).
    con.execute(f"""
        CREATE TEMP TABLE tail AS
        SELECT g.asset_id, g.delist_date, g.status, g.delisting_date, g.last_bar,
               (SELECT close FROM bars b WHERE b.asset_id = g.asset_id AND b.date = g.last_bar) AS last_close,
               (SELECT max(close) FROM (SELECT close FROM bars b WHERE b.asset_id = g.asset_id ORDER BY date DESC LIMIT 60)) AS max60,
               {sym_at.format(a='g.asset_id', d='g.delist_date')} AS symbol
        FROM gone g
    """)
    con.execute("""
        CREATE TEMP TABLE delist AS
        SELECT *, CASE WHEN regexp_matches(symbol, 'Q(-\\d{6})?$') AND length(regexp_replace(symbol, '-\\d{6}$', '')) >= 4 THEN 'bankruptcy'
                       WHEN last_close < 1 THEN 'bankruptcy'
                       WHEN last_close < 0.5 * max60 THEN 'other'
                       ELSE 'acquisition' END AS reason
        FROM tail
    """)
    report["delisting_reason_histogram"] = dict(q("SELECT reason, count(*) FROM delist GROUP BY 1 ORDER BY 2 DESC"))
    report["delisting_status_histogram"] = {str(k): v for k, v in q("SELECT coalesce(status, 'unknown'), count(*) FROM delist GROUP BY 1")}
    report["delisting_date_vs_master"] = {
        "master_date_equals_last_bar": one("SELECT count(*) FROM delist WHERE delisting_date = last_bar"),
        "master_date_differs": one("SELECT count(*) FROM delist WHERE delisting_date IS NOT NULL AND delisting_date <> last_bar"),
        "stops_trading_but_master_active": one("SELECT count(*) FROM delist WHERE status = 'active'"),
    }
    report["written"]["delistings.csv"] = copy("SELECT symbol, delist_date AS date, reason FROM delist ORDER BY date, symbol", "delistings.csv")

    # 6. Membership: half-open [member_from, member_to) in the lake, inclusive
    # in the adapter, clipped to each symbol interval and to the asset's bars,
    # and split around trading days on which the asset has no bar (a halt):
    # the bundle's membership test wants every member in the universe.
    con.execute(f"""
        CREATE TEMP TABLE mem0 AS
        SELECT i.ticker AS symbol,
               greatest(m.member_from, i."from", s.first_bar) AS f,
               least(coalesce((SELECT prev FROM nextday WHERE date = m.member_to), m.member_to - 1, DATE '9999-12-31'),
                     coalesce(i."to", DATE '9999-12-31'), s.last_bar) AS t,
               m.member_to IS NULL AS open_ended, i.asset_id
        FROM members m JOIN ids USING (asset_id) JOIN intervals i USING (asset_id) JOIN asset_span s USING (asset_id)
    """)
    con.execute("CREATE TEMP TABLE dayno AS SELECT date, row_number() OVER (ORDER BY date) AS n FROM days")
    con.execute("""
        CREATE TEMP TABLE mem AS
        WITH member_bars AS (
          SELECT m.symbol, m.f, m.open_ended, b.date, d.n,
                 d.n - row_number() OVER (PARTITION BY m.symbol, m.f ORDER BY b.date) AS island
          FROM mem0 m JOIN bars b ON b.asset_id = m.asset_id AND b.date BETWEEN m.f AND m.t
          JOIN dayno d ON d.date = b.date)
        SELECT symbol, min(date) AS f, max(date) AS t, bool_and(open_ended) AS open_ended
        FROM member_bars GROUP BY symbol, f, island
    """)
    report["membership_intervals_split_at_missing_bars"] = one("SELECT count(*) FROM mem") - one("SELECT count(*) FROM mem0 WHERE f <= t")
    report["written"]["membership.csv"] = copy(
        f"""SELECT symbol, '{args.label}' AS "index", f AS "from",
                   CASE WHEN open_ended AND t >= DATE '{last_day}' THEN NULL ELSE t END AS "to"
            FROM mem WHERE f <= t ORDER BY symbol, f""", "membership.csv")

    # 7. Classification: one snapshot, emitted from its snapshot date onward.
    snap = one(f"SELECT max(snapshot_date) FROM read_parquet({p('classification/classification_full/data.parquet')}) WHERE scheme = '{args.scheme}'")
    report["classification_snapshot"] = str(snap)
    report["written"]["classification.csv"] = copy(
        f"""SELECT i.ticker AS symbol, c.scheme, c.code, greatest(c.snapshot_date, i."from") AS "from", i."to"
            FROM read_parquet({p('classification/classification_full/data.parquet')}) c
            JOIN ids USING (asset_id) JOIN intervals i USING (asset_id)
            WHERE c.scheme = '{args.scheme}' AND c.level = 1 AND c.snapshot_date <= DATE '{last_day}'
              AND (i."to" IS NULL OR i."to" >= c.snapshot_date)
            ORDER BY symbol""", "classification.csv")

    if args.exceptions:
        rows = Path(args.exceptions).read_text().splitlines()
        if not rows or rows[0].strip() != "test,symbol,date,reason":
            log(f"error: {args.exceptions} must start with the header test,symbol,date,reason")
            return 2
        (out / "exceptions.csv").write_text("\n".join(rows) + "\n")
        report["written"]["exceptions.csv"] = len(rows) - 1
        log(f"wrote exceptions.csv: {len(rows) - 1} reviewed exceptions from {args.exceptions}")

    report["decisions"] = [
        "prices are Norgate RAW (as traded); the lake's Phase-0 gate refuses adjusted series",
        "split factor = Norgate split_ratio (new shares per old, e.g. 4.0 for AAPL 2020-08-31)",
        "dividends: announce_date = pay_date = ex_date (Norgate has neither)",
        "delisting reason inferred: ticker ending in Q or last close < $1 -> bankruptcy; last close < 50% of the 60-bar max -> other; else acquisition",
        "delisting date = first trading day after the last print",
        f"classification: {args.scheme} level 1 from the single snapshot {snap}, emitted only from that date",
        "membership: lake half-open intervals converted to inclusive, clipped to the asset's bars, split around halts",
        "a dividend on the ex-date of a split is divided by the split factor (Norgate quotes it per pre-split share)",
    ]
    report["seconds"] = round(time.time() - started, 1)
    (out / "ingest-report.json").write_text(json.dumps(report, indent=2, default=str))
    log(json.dumps(report, indent=2, default=str))
    return 0


if __name__ == "__main__":
    sys.exit(main())
