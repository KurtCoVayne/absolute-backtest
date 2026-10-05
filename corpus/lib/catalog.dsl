# Catalog library (data-bundle doc, section 3): what the catalog stores as
# traded, derived causally. `ret` applies the split factor and the cash
# dividend at the ex-date; `close_adj` carries the cumulative split factor
# forward (WF-4), so it is adjusted with events at or before T only and is
# continuous across splits in the first bar's share basis (use `close` for a
# price level, `close_adj` for a comparison across time).
library catalog {
  env equities_1d_v2
  resolution @1d

  rel bar(@T: Timestamp)
  bar(T) :- universe(_, T).

  # The split factor at T, or one.
  rel split_or_one(+A: Equity, @T: Timestamp, -F: Scalar)
  split_or_one(A, T, F) :- universe(A, T), split(A, T, F).
  split_or_one(A, T, 1) :- universe(A, T), not split(A, T, _).

  # The dividend going ex at T, announced at or before T, or zero.
  rel div_at(+A: Equity, @T: Timestamp, -D: Price<USD>)
  div_at(A, T, D) :- universe(A, T),
      D = sum(Amt) over (T0 in window(T, 1y, min 1), dividend(A, T0, Ex, Pay, Amt), Ex = T).
  rel div_or_zero(+A: Equity, @T: Timestamp, -D: Price<USD>)
  div_or_zero(A, T, D) :- div_at(A, T, D).
  div_or_zero(A, T, 0 USD/share) :- universe(A, T), not div_at(A, T, _).

  # Total return from the previous close: (P * F + D) / P0 - 1.
  rel ret(+A: Equity, @T: Timestamp, -R: Scalar)
  ret(A, T, R) :- close(A, T, P), prev(T, T0), close(A, T0, P0),
      split_or_one(A, T, F), div_or_zero(A, T, D), R = (P * F + D) / P0 - 1.

  # The cumulative split factor since the first bar A was listed on.
  rel listed_before(+A: Equity, @T: Timestamp)
  listed_before(A, T) :- universe(A, T), prev(T, T0), universe(A, T0).
  rel cumfactor(+A: Equity, @T: Timestamp, -C: Scalar)
  cumfactor(A, T, C) :- universe(A, T), not listed_before(A, T), split_or_one(A, T, C).
  cumfactor(A, T, C) :- listed_before(A, T), prev(T, T0), cumfactor(A, T0, C0), split_or_one(A, T, F), C = C0 * F.

  # Point-in-time adjusted close: the as-traded close scaled by the splits
  # at or before T, so that it compares across time.
  rel close_adj(+A: Equity, @T: Timestamp, -P: Price<USD>)
  close_adj(A, T, P) :- close(A, T, P0), cumfactor(A, T, C), P = P0 * C.

  rel logret(+A: Equity, @T: Timestamp, -R: Scalar)
  logret(A, T, R) :- ret(A, T, R0), R = log(1 + R0).

  # Average daily dollar volume, and the liquidity filter of the doc.
  rel adv(+A: Equity, @T: Timestamp, +N: Duration, +K: Count, -D: Notional<USD>)
  adv(A, T, N, K, D) :- universe(A, T),
      D = mean(P * V) over (T1 in window(T, N, min K), close(A, T1, P), volume(A, T1, V)).
  rel liquid(+A: Equity, @T: Timestamp, +MinAdv: Notional<USD>)
  liquid(A, T, MinAdv) :- adv(A, T, 20d, 10, D), D >= MinAdv.

  rel held(-A: Equity, @T: Timestamp, -Q: Quantity<Shares>)
  held(A, T, Q) :- position(A, T, Q), Q > 0 shares.
  rel flat(+A: Equity, @T: Timestamp)
  flat(A, T) :- universe(A, T), not position(A, T, _).
}
