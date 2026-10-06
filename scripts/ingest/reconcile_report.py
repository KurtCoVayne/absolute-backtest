#!/usr/bin/env python3
"""The bundle test's action reconciliation (src/bundle.rs::run_tests), run on
a `--from-norgate` CSV directory and printed in full rather than the first
five problems, each classified so a person can decide whether the data or
the test is wrong:

  split_without_move   a split on a bar where the close did not move by the factor
  jump_with_dividend   an unexplained jump on an ex-date carrying a large dividend
                       (Norgate's spin-off/special encoding)
  jump_reverting       an unexplained jump undone within five bars (bad print or squeeze)
  jump                 any other unexplained jump (real event or missing split)

Same rule as the test: the total-return ratio (close + dividends) x split factor /
previous close is within 25% of one on a split's ex-date, within [0.6, 1.67] otherwise.
"""
import argparse
import sys

import duckdb


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("dir")
    ap.add_argument("--csv", help="write every problem to this CSV")
    ap.add_argument("--propose", help="write a DRAFT exceptions file (test,symbol,date,reason) for a person to review")
    args = ap.parse_args()
    d = args.dir.rstrip("/")
    con = duckdb.connect()
    con.execute(f"""
        CREATE TABLE px AS SELECT symbol, date, close,
            lag(close) OVER w AS prev, lead(close, 5) OVER w AS ahead5 FROM read_csv('{d}/prices.csv', header=true)
            WINDOW w AS (PARTITION BY symbol ORDER BY date)""")
    con.execute(f"CREATE TABLE sp AS SELECT * FROM read_csv('{d}/splits.csv', header=true)")
    con.execute(f"CREATE TABLE dv AS SELECT symbol, ex_date, sum(amount) amount FROM read_csv('{d}/dividends.csv', header=true) GROUP BY ALL")
    con.execute("""
        CREATE TABLE problems AS
        SELECT p.symbol, p.date, p.prev, p.close, (p.close + coalesce(dv.amount, 0)) / p.prev AS ratio, s.factor, dv.amount AS dividend,
          CASE
            WHEN s.factor IS NOT NULL THEN 'split_without_move'
            WHEN dv.amount IS NOT NULL AND dv.amount > 0.2 * p.prev THEN 'jump_with_dividend'
            WHEN p.ahead5 IS NOT NULL AND abs(ln(p.ahead5 / p.prev)) < 0.2 THEN 'jump_reverting'
            ELSE 'jump' END AS kind
        FROM px p LEFT JOIN sp s ON s.symbol = p.symbol AND s.ex_date = p.date
                  LEFT JOIN dv ON dv.symbol = p.symbol AND dv.ex_date = p.date
        WHERE p.prev IS NOT NULL AND (
          (s.factor IS NOT NULL AND abs((p.close + coalesce(dv.amount, 0)) / p.prev * s.factor - 1) > 0.25)
          OR (s.factor IS NULL AND ((p.close + coalesce(dv.amount, 0)) / p.prev < 0.6 OR (p.close + coalesce(dv.amount, 0)) / p.prev > 1.67)))
        ORDER BY p.date, p.symbol""")
    print(con.sql("SELECT kind, count(*) n, count(DISTINCT symbol) symbols FROM problems GROUP BY 1 ORDER BY 2 DESC"))
    print(con.sql("SELECT symbol, count(*) n, min(date) AS first_date, max(date) AS last_date FROM problems GROUP BY 1 ORDER BY 2 DESC LIMIT 25"))
    print(con.sql("SELECT * FROM problems WHERE kind <> 'jump_reverting' ORDER BY kind, date LIMIT 80").df().to_string())
    if args.csv:
        con.execute(f"COPY problems TO '{args.csv}' (HEADER)")
    if args.propose:
        # Reasons by category; every row is a claim a person must check.
        con.execute(f"CREATE TABLE gone AS SELECT * FROM read_csv('{d}/delistings.csv', header=true)")
        con.execute(f"""
            COPY (
              SELECT 'action reconciliation' AS test, p.symbol, p.date,
                CASE
                  WHEN p.kind = 'split_without_move' THEN 'split with a same-day price move of ' || round(100 * (p.ratio * p.factor - 1)) || '% after the factor; kept as traded'
                  WHEN g.reason = 'bankruptcy' OR p.symbol IN ('FRCB', 'SBNY') OR p.prev < 1
                    THEN 'distressed or post-failure trading (' || coalesce(g.reason, 'sub-dollar') || '); move of ' || round(100 * (p.ratio - 1)) || '% is as traded'
                  WHEN p.date BETWEEN DATE '2020-03-01' AND DATE '2020-04-30' THEN 'COVID-19 and oil-price crash; move of ' || round(100 * (p.ratio - 1)) || '%'
                  WHEN p.kind = 'jump_reverting' THEN 'move of ' || round(100 * (p.ratio - 1)) || '% reversed within five bars (squeeze or print); kept as traded'
                  WHEN p.kind = 'jump_with_dividend' THEN 'distribution of ' || p.dividend || ' does not explain a move of ' || round(100 * (p.ratio - 1)) || '%'
                  ELSE 'single-name event move of ' || round(100 * (p.ratio - 1)) || '% with no split in the vendor table'
                END AS reason
              FROM problems p LEFT JOIN gone g ON g.symbol = p.symbol ORDER BY p.date, p.symbol
            ) TO '{args.propose}' (HEADER, QUOTE '', ESCAPE '', DATEFORMAT '%Y-%m-%d')""")
        print(f"draft exceptions written to {args.propose}; review every row before using it", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
