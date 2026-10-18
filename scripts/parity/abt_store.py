"""A run's outputs read from the data layer's store (K-7.1, K-9.1): every
relation through `abt data query --store STORE 'kernel.<run>.<relation>'
--parquet FILE`, into pandas, times as naive UTC timestamps.

The `abt` binary is $ABT, else target/release/abt in the repository, else
`abt` on the PATH. The run is the id `abt run --data SOCKET` prints (its
outputs are written to the store the data service holds):

  abt data ingest --store STORE --adapter bundle_v2 --from BUNDLE --class C
  abt data serve --store STORE --socket SOCKET &
  abt run --strategy NAME --data SOCKET <files...>      # prints `run <id>: ...`
"""
from __future__ import annotations

import json
import os
import shutil
import subprocess
import tempfile

import numpy as np
import pandas as pd

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))


def abt() -> str:
    if os.environ.get("ABT"):
        return os.environ["ABT"]
    built = os.path.join(ROOT, "target", "release", "abt")
    if os.path.exists(built):
        return built
    return shutil.which("abt") or "abt"


def query(store: str, text: str) -> pd.DataFrame:
    """The answer to a query in the text form of D-4.5."""
    with tempfile.TemporaryDirectory() as d:
        out = os.path.join(d, "answer.parquet")
        p = subprocess.run([abt(), "data", "query", "--store", store, text, "--parquet", out], capture_output=True, text=True)
        if p.returncode != 0:
            raise SystemExit(f"abt data query {text!r}: {p.stderr.strip()}")
        df = pd.read_parquet(out)
    for c in df.columns:
        if isinstance(df[c].dtype, pd.DatetimeTZDtype):
            df[c] = df[c].dt.tz_convert(None)
    return df


def relation(store: str, run: str, name: str) -> pd.DataFrame:
    """`kernel.<run>.<name>`: events, book, holdings, decisions, fills, ..."""
    return query(store, f"kernel.{run}.{name}")


def keys(store: str) -> dict[int, str]:
    """The instruments' keys by their ids (the registry's `vendor_id`)."""
    v = query(store, "vendor_id")
    return dict(zip(v["id"].astype("int64"), v["key"]))


def report(store: str, run: str) -> dict:
    """The run's report rows, their JSON values decoded."""
    r = relation(store, run, "report")
    return {k: json.loads(v) for k, v in zip(r["key"], r["value"])}


def bar_returns(book: pd.DataFrame, base: float | None) -> pd.DataFrame:
    """The book's return at every decision barrier after the first: the NAV
    change over the fixed base, or over the previous NAV when returns
    compound (RunResult::bar_returns), with the P&L."""
    b = book.sort_values("t")
    nav = b["nav"].to_numpy()
    pnl = np.diff(nav)
    denom = np.full(len(pnl), base) if base is not None else nav[:-1]
    ret = np.where(denom > 0, pnl / np.where(denom > 0, denom, 1.0), 0.0)
    return pd.DataFrame({"t": b["t"].to_numpy()[1:], "ret": ret, "pnl": pnl})


def daily_returns(book: pd.DataFrame, base: float | None, calendar_days=None) -> pd.DataFrame:
    """The returns per day (period_returns_on with a daily report calendar):
    the bars' returns summed (fixed base) or compounded, by UTC day; with a
    calendar, its days from the first bar's day to the last's, 0 where no
    bar falls."""
    bars = bar_returns(book, base)
    bars["day"] = bars["t"].dt.normalize()
    if base is not None:
        days = bars.groupby("day").agg(ret=("ret", "sum"), pnl=("pnl", "sum"))
    else:
        days = bars.groupby("day").agg(ret=("ret", lambda x: np.prod(1 + x) - 1), pnl=("pnl", "sum"))
    if calendar_days is None:
        return days.reset_index().rename(columns={"day": "t"})
    t = book["t"]
    lo, hi = t.min().normalize(), t.max().normalize()
    cal = sorted(d for d in set(pd.DatetimeIndex(calendar_days).normalize()) if lo <= d <= hi)
    out = days.reindex(cal).fillna(0.0)
    out.index.name = "t"
    return out.reset_index()


def session_days(store: str, root: str, class_: str = "future") -> list:
    """The days of a root's sessions (`sessions of "ROOT"`): each session's
    label (its close) as a UTC day."""
    s = query(store, f"{class_}.session")
    ids = {k: i for i, k in keys(store).items()}
    if root not in ids:
        raise SystemExit(f"`{root}` is no instrument key of the store")
    return sorted(set(s.loc[s["A"].astype("int64") == ids[root], "S"].dt.normalize()))
