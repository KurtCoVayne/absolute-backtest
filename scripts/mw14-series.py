#!/usr/bin/env python3
"""scripts/mw14-series.py LAKE BOOK_SPXTR BOOK_VIX CBOE_CSV OUT_DIR

MW14's two series carried to a newer lake's last day, each from the source the book's own file was made from, so
that scripts/bench-mw14.sh can run the book to the end of a lake newer than the book's files.

  OUT_DIR/spxtr_daily.parquet  the book's file, then the lake's $SPXTR bars of the days after its last. The two
                               must agree on every day they share, or nothing is written.
  OUT_DIR/vix_weekly.parquet   the book's file to its last whole week, then CBOE's VIX history: Monday-truncated
                               weeks, the week's last close, the 4-week mean (the convention of the book's
                               strategy/ivmom/vix.py). The book's file was frozen in the middle of its last week,
                               so that week is CBOE's. The history is cut at the lake's last $SPXTR day, so the
                               last week holds what that day knew. Before that week the two must agree on every
                               week, or nothing is written. The book's other columns (the percentile, VIX3M,
                               VVIX), which MW14 does not read, are empty in the weeks added.

CBOE_CSV is https://cdn.cboe.com/api/global/us_indices/daily_prices/VIX_History.csv as downloaded (DATE, OPEN,
HIGH, LOW, CLOSE). The lake's own $VIX is not used: its weekly last close differs from CBOE's in 11 of the book's
1,907 weeks (by up to 2.04), CBOE's in none before the frozen week."""
import os
import sys

import pandas as pd
import pyarrow.dataset as ds

lake, book_spxtr, book_vix, cboe_csv, out = sys.argv[1:6]
os.makedirs(out, exist_ok=True)

# $SPXTR: the book's days, then the lake's.
b = pd.read_parquet(book_spxtr)
b["date"] = pd.to_datetime(b["date"])
sm = pd.read_parquet(os.path.join(lake, "registry", "security_master", "data.parquet"), columns=["asset_id", "ticker"])
ids = sm[sm.ticker == "$SPXTR"].asset_id.unique()
assert len(ids) == 1, f"the lake's security master gives $SPXTR {len(ids)} asset ids"
bars = ds.dataset(os.path.join(lake, "curated", "bars_by_date"), format="parquet", partitioning="hive")
cols = ["date", "open", "high", "low", "close", "volume"]
x = bars.to_table(filter=ds.field("asset_id") == int(ids[0]), columns=cols).to_pandas()
x["date"] = pd.to_datetime(x["date"])
# A lake with an `archive` partition repeats some days before 1990: a day's rows must be one bar.
x = x.drop_duplicates(subset=cols).sort_values("date")
assert x.date.is_unique, "the lake gives $SPXTR two different bars on one day"
mine = b[b.symbol == "$SPXTR"]
both = mine.merge(x, on="date", suffixes=("", "_lake"))
off = both[(both.close.astype("float64") - both.close_lake.astype("float64")).abs() > 1e-6 * both.close.abs()]
assert len(both) > 0 and len(off) == 0, f"$SPXTR: the book's file and the lake differ on {len(off)} of {len(both)} days"
add = x[x.date > mine.date.max()].assign(symbol="$SPXTR")[list(b.columns)]
spx = pd.concat([b, add], ignore_index=True).astype(dict(b.dtypes))
spx.to_parquet(os.path.join(out, "spxtr_daily.parquet"), index=False)
last = spx[spx.symbol == "$SPXTR"].date.max()
print(f"$SPXTR: {len(mine)} days of the book's (to {mine.date.max().date()}), equal to the lake's on the {len(both)} they share; {len(add)} added, to {last.date()}")

# The weekly VIX: the book's weeks, then CBOE's.
v = pd.read_parquet(book_vix)
v["wk"] = pd.to_datetime(v["wk"])
c = pd.read_csv(cboe_csv)
c.columns = [n.strip().lower() for n in c.columns]
c["date"] = pd.to_datetime(c["date"], format="%m/%d/%Y")
c = c[c.date <= last].sort_values("date")
c["wk"] = (c.date - pd.to_timedelta(c.date.dt.day_of_week, unit="D")).dt.normalize()
wk = c.groupby("wk").close.last().round(2).rename("vix").reset_index()
wk["vix_ma4"] = wk.vix.rolling(4).mean()
frozen = v.wk.max()
m = v[v.wk < frozen].merge(wk, on="wk", suffixes=("", "_cboe"))
off = m[((m.vix - m.vix_cboe).abs() > 0.005) | ((m.vix_ma4 - m.vix_ma4_cboe).abs() > 0.005)]
assert len(m) == len(v) - 1 and len(off) == 0, f"the weekly VIX: the book's file and CBOE differ in {len(off)} of {len(m)} weeks"
keep, new = v[v.wk < frozen], wk[wk.wk >= frozen]
o = pd.concat([keep, new], ignore_index=True)
o.to_parquet(os.path.join(out, "vix_weekly.parquet"), index=False)
print(f"weekly VIX: {len(keep)} weeks of the book's (to {keep.wk.max().date()}), equal to CBOE's in each; {len(new)} of CBOE's added, {new.wk.min().date()} to {new.wk.max().date()}")
