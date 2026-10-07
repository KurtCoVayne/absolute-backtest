#!/usr/bin/env python3
"""Show and edit a reviewed bundle-test exceptions file (Parquet with the
columns test,symbol,date,reason; see `abt bundle build --from-norgate`).

  exceptions_edit.py FILE show [--symbol S] [--grep TEXT]
  exceptions_edit.py FILE add --test T --symbol S --date YYYY-MM-DD --reason TEXT
  exceptions_edit.py FILE remove --symbol S --date YYYY-MM-DD [--test T]
  exceptions_edit.py FILE merge DRAFT.parquet     (rows of a reviewed draft, e.g. from reconcile_report.py --propose)

Every row is a claim a person checked: the reason says why the data is right
and the test too strict there. A row without a reason is refused.
"""
import argparse
import sys
from pathlib import Path

import duckdb

COLUMNS = ["test", "symbol", "date", "reason"]


def load(con: duckdb.DuckDBPyConnection, path: Path) -> None:
    if path.exists():
        con.execute(f"CREATE TABLE ex AS SELECT test, symbol, CAST(date AS DATE) AS date, reason FROM read_parquet('{path}')")
    else:
        con.execute("CREATE TABLE ex (test VARCHAR, symbol VARCHAR, date DATE, reason VARCHAR)")


def save(con: duckdb.DuckDBPyConnection, path: Path) -> None:
    bad = con.execute("SELECT count(*) FROM ex WHERE reason IS NULL OR trim(reason) = ''").fetchone()[0]
    if bad:
        sys.exit(f"error: {bad} row(s) without a reason")
    tmp = path.with_suffix(".tmp.parquet")
    con.execute(f"COPY (SELECT DISTINCT * FROM ex ORDER BY date, symbol, test) TO '{tmp}' (FORMAT parquet)")
    tmp.replace(path)


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("file", type=Path)
    sub = ap.add_subparsers(dest="cmd", required=True)
    show = sub.add_parser("show")
    show.add_argument("--symbol")
    show.add_argument("--grep")
    add = sub.add_parser("add")
    for a in ("--test", "--symbol", "--date", "--reason"):
        add.add_argument(a, required=True)
    rm = sub.add_parser("remove")
    rm.add_argument("--symbol", required=True)
    rm.add_argument("--date", required=True)
    rm.add_argument("--test")
    merge = sub.add_parser("merge")
    merge.add_argument("draft", type=Path)
    args = ap.parse_args()
    con = duckdb.connect()
    load(con, args.file)
    if args.cmd == "show":
        where, params = ["TRUE"], []
        if args.symbol:
            where.append("symbol = ?")
            params.append(args.symbol)
        if args.grep:
            where.append("reason ILIKE ?")
            params.append(f"%{args.grep}%")
        rows = con.execute(f"SELECT * FROM ex WHERE {' AND '.join(where)} ORDER BY date, symbol", params).fetchall()
        for r in rows:
            print(f"{r[2]}  {r[1]:<8} {r[0]}: {r[3]}")
        print(f"{len(rows)} row(s)", file=sys.stderr)
        return 0
    if args.cmd == "add":
        con.execute("INSERT INTO ex VALUES (?, ?, CAST(? AS DATE), ?)", [args.test, args.symbol, args.date, args.reason])
    elif args.cmd == "remove":
        q = "DELETE FROM ex WHERE symbol = ? AND date = CAST(? AS DATE)"
        params = [args.symbol, args.date]
        if args.test:
            q += " AND test = ?"
            params.append(args.test)
        n = con.execute(q, params).fetchone()[0]
        print(f"removed {n} row(s)", file=sys.stderr)
    elif args.cmd == "merge":
        con.execute(f"INSERT INTO ex SELECT test, symbol, CAST(date AS DATE), reason FROM read_parquet('{args.draft}')")
    save(con, args.file)
    print(f"{args.file}: {con.execute('SELECT count(DISTINCT (test, symbol, date, reason)) FROM ex').fetchone()[0]} row(s)", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
