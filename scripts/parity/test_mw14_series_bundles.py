#!/usr/bin/env python3
"""Tests of the frames scripts/parity/mw14_series_bundles.py stages and of
the alignment scripts/parity/mw14_compare.py makes, on small frames built
here (nothing is read from the lake or the book):

- a week whose Friday is a holiday is keyed at its Thursday, the label the
  kernel gives it, so the label reads that week's VIX and not the previous
  week's (ERRATA E-147);
- the gate's index and its 52-week mean run from the series' first week, so
  both exist at the first week of the equity data a year later (E-146);
- with polars importable, the gate frame equals the book's own polars code
  (`data.py` `attach_gate_index`, `book.py` 331-335) to the bit;
- the comparison drops the weeks decided before the canon's first and
  compares from there;
- the comparison's check that every run label but the last is a weekly
  series key (E-147).

  python3 scripts/parity/test_mw14_series_bundles.py

It needs numpy, pandas and pyarrow (what the two scripts need); without
pandas it says so and exits 0.
"""
import os
import sys
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)

try:
    import numpy as np
    import pandas as pd
except ImportError as e:
    print(f"skipped: {sys.executable} cannot import numpy and pandas ({e})")
    sys.exit(0)

import mw14_compare  # noqa: E402
import mw14_series_bundles as sb  # noqa: E402


def daily(start: str, days: int, holidays: tuple = ()) -> pd.DataFrame:
    """`$SPXTR` export rows on the business days from `start`, but
    `holidays`, with a close that moves every day; one `$SPX` row a day
    besides, which the script must ignore."""
    dates = [d for d in pd.bdate_range(start, periods=days) if str(d.date()) not in holidays]
    rng = np.random.default_rng(7)
    close = (250.0 * np.exp(np.cumsum(0.01 * rng.standard_normal(len(dates))))).astype("float32")
    spxtr = pd.DataFrame({"date": dates, "symbol": "$SPXTR", "close": close})
    spx = pd.DataFrame({"date": dates, "symbol": "$SPX", "close": close / 2})
    return pd.concat([spxtr, spx], ignore_index=True)


class Weekly(unittest.TestCase):
    def test_a_holiday_week_is_keyed_at_its_last_trading_day(self):
        # Friday 2008-03-21 (Good Friday) is no trading day.
        d = sb.spxtr_daily(daily("2008-03-10", 15, holidays=("2008-03-21",)))
        ends = sb.week_ends(d)
        self.assertEqual([str(x.date()) for x in ends], ["2008-03-14", "2008-03-20", "2008-03-28"])
        vix = pd.DataFrame({"wk": pd.to_datetime(["2008-03-10", "2008-03-17", "2008-03-24"]), "vix": [25.0, 26.0, 27.0], "vix_ma4": [np.nan, 24.0, 25.0]})
        gate = sb.gate_index(d)
        rows, unkeyed = sb.weekly_rows(gate, vix, ends)
        self.assertEqual(unkeyed, 0)
        at = lambda name: {str(t.date()): v for t, v in zip(rows[rows["Name"] == name]["T"], rows[rows["Name"] == name]["V"])}
        self.assertEqual(at("VIX"), {"2008-03-14": 25.0, "2008-03-20": 26.0, "2008-03-28": 27.0})
        # A missing value has no row: the as-of read takes the week before.
        self.assertEqual(at("VIX_MA4"), {"2008-03-20": 24.0, "2008-03-28": 25.0})
        self.assertEqual(sorted(at("SPXTR_W")), ["2008-03-14", "2008-03-20", "2008-03-28"])
        self.assertEqual(at("SPXTR_W_MA52"), {}, "three weeks hold no 52-week mean")

    def test_a_vix_week_without_an_index_date_is_keyed_at_its_friday(self):
        d = sb.spxtr_daily(daily("2008-03-10", 5))
        vix = pd.DataFrame({"wk": pd.to_datetime(["2008-03-10", "2008-03-17"]), "vix": [25.0, 26.0], "vix_ma4": [24.0, 25.0]})
        rows, unkeyed = sb.weekly_rows(sb.gate_index(d), vix, sb.week_ends(d))
        self.assertEqual(unkeyed, 1)
        self.assertEqual(sorted(str(t.date()) for t in rows[rows["Name"] == "VIX"]["T"]), ["2008-03-14", "2008-03-21"])

    def test_the_gate_holds_from_the_first_week_a_year_after_the_series_starts(self):
        # From Monday 1988-01-04, as the book's export; the equities start in 1990.
        raw = daily("1988-01-04", 640)
        d = sb.spxtr_daily(raw)
        g = sb.gate_index(d)
        self.assertTrue(np.isnan(g["ma"].iloc[50]))
        self.assertFalse(np.isnan(g["ma"].iloc[51]))
        first_1990 = g[g["week_end"] >= pd.Timestamp("1990-01-01")].iloc[0]
        self.assertEqual(str(first_1990["week_end"].date()), "1990-01-05")
        self.assertFalse(np.isnan(first_1990["ma"]))
        # The index by hand: float32 ratios, their weekly product and its
        # running product, in date order; the first day has no ratio.
        c = d["close"].to_numpy(np.float32)
        wk = sb.week_of(d["date"]).to_numpy()
        tri, acc, f = [], np.float32(1.0), np.float32(1.0)
        for i in range(len(c)):
            if i > 0:
                f = np.float32(f * np.float32(c[i] / c[i - 1]))
            if i + 1 == len(c) or wk[i + 1] != wk[i]:
                acc = np.float32(acc * f)
                tri.append(float(acc))
                f = np.float32(1.0)
        self.assertEqual(g["tri"].tolist(), tri)
        k = 60
        self.assertEqual(g["ma"].iloc[k], sum(tri[k - 51 : k + 1]) / 52)
        # The index is proportional to the close: the week's last close over the first day's.
        last = d.groupby(sb.week_of(d["date"]))["close"].last().to_numpy(np.float64)
        np.testing.assert_allclose(g["tri"].to_numpy(), last / float(c[0]), rtol=1e-5)

    def test_the_gate_frame_is_the_books_polars_code_to_the_bit(self):
        try:
            import polars as pl
        except ImportError:
            self.skipTest("polars is not importable: the book's own code is not run")
        raw = daily("1988-01-04", 640, holidays=("1988-04-01", "1989-03-24"))
        d = (
            pl.from_pandas(raw)
            .filter(pl.col("symbol") == "$SPXTR")
            .with_columns(pl.col("date").cast(pl.Date), pl.col("close").cast(pl.Float32))
            .sort("date")
            .with_columns((pl.col("close") / pl.col("close").shift(1)).alias("trf"), pl.col("date").dt.truncate("1w").alias("wk"))
        )
        book = (
            d.group_by("wk")
            .agg(pl.col("date").last().alias("week_end"), pl.col("trf").product().alias("wk_trf"))
            .sort("wk")
            .with_columns(pl.col("wk_trf").fill_null(1.0).cum_prod().alias("tri"))
            .with_columns(pl.col("tri").cast(pl.Float64))
            .with_columns(pl.col("tri").rolling_mean(52).alias("ma"))
        )
        g = sb.gate_index(sb.spxtr_daily(raw))
        self.assertEqual([str(x) for x in book["week_end"].to_list()], [str(x.date()) for x in g["week_end"]])
        self.assertEqual(book["tri"].to_numpy().tobytes(), g["tri"].to_numpy().tobytes())
        ma = book["ma"].to_numpy()
        m = ~np.isnan(ma)
        self.assertTrue((m == ~np.isnan(g["ma"].to_numpy())).all())
        self.assertEqual(ma[m].tobytes(), g["ma"].to_numpy()[m].tobytes())


class Compare(unittest.TestCase):
    def test_the_weeks_before_the_canons_first_are_not_compared(self):
        t = pd.to_datetime(["1990-12-21", "1990-12-28", "1991-01-04", "1991-01-11", "1991-01-18"])
        book = pd.DataFrame({"t": t, "nav": [1e6, 1e6, 1e6, 1.01e6, 1.0e6]})
        before, after = mw14_compare.align(book, 1e6, pd.Timestamp("1991-01-04"))
        self.assertEqual([str(x.date()) for x in before["decided"]], ["1990-12-21", "1990-12-28"])
        self.assertEqual(np.abs(before["ret"]).sum(), 0.0)
        self.assertEqual([str(x.date()) for x in after["decided"]], ["1991-01-04", "1991-01-11"])
        self.assertEqual(after["ret"].tolist(), [0.01, -0.01])
        self.assertEqual([str(x.date()) for x in after["t"]], ["1991-01-11", "1991-01-18"])

    def test_every_label_but_the_last_is_a_weekly_key(self):
        # Friday 2026-06-19 is a holiday: that week's label and key are its
        # Thursday. The equities end on Wednesday 2026-06-24 and the index
        # runs on, so the last week is keyed at its Friday (E-147).
        labels = pd.Series(pd.to_datetime(["2026-06-05", "2026-06-12", "2026-06-18", "2026-06-24"]))
        keys = pd.Series(pd.to_datetime(["2026-06-05", "2026-06-12", "2026-06-18", "2026-06-26", "2026-07-02"]))
        off, last = mw14_compare.label_key_misses(labels, keys)
        self.assertEqual(off, [])
        self.assertEqual(str(last.date()), "2026-06-24")
        # Under the Friday keying the holiday week's label is off its key.
        fridays = pd.Series(pd.to_datetime(["2026-06-05", "2026-06-12", "2026-06-19", "2026-06-24"]))
        off, last = mw14_compare.label_key_misses(labels, fridays)
        self.assertEqual([str(x.date()) for x in off], ["2026-06-18"])
        self.assertIsNone(last)


if __name__ == "__main__":
    unittest.main(verbosity=2)
