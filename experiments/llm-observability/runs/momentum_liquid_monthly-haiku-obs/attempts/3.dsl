# momentum_liquid_monthly, attempt 3 (final).
# Rebalance on the first bar of each calendar month. Candidates: universe names
# that are index members (member, label "SPX"), pass the liquidity screen
# (catalog liquid: average daily dollar volume over 20d with at least 10 bars,
# >= min_adv) and have 12-1 total-return momentum: the total-return index tri
# (catalog ret, dividends included) at lag(skip) over the index at lag(lookback).
# The n_hold strongest are held at equal target weight 1/n_hold; held names that
# are not selected are targeted to zero at the same rebalance. Nothing else trades.
strategy momentum_liquid_monthly {
  env equities_1d_v2
  uses catalog
  resolution @1d
  mode target

  param n_hold : Count = 10
  param min_adv : Notional<USD> = 1_000_000 USD
  param lookback : Duration = 12mo
  param skip : Duration = 1mo

  # Rebalance dates: the first bar of each calendar month.
  rel mstart(@T: Timestamp)
  mstart(T) :- bar(T), month_start(T).

  # Total-return index: product of (1 + total return) from the first bar A is listed.
  rel tri(+A: Equity, @T: Timestamp, -I: Scalar)
  tri(A, T, I) :- universe(A, T), not listed_before(A, T), I = 1.
  tri(A, T, I) :- listed_before(A, T), prev(T, T0), tri(A, T0, I0), ret(A, T, R), I = I0 * (1 + R).

  # Total return from `lookback` ago to `skip` ago (skips the most recent month).
  rel mom(+A: Equity, @T: Timestamp, -M: Scalar)
  mom(A, T, M) :- universe(A, T), lag(T, skip, T1), tri(A, T1, I1),
      lag(T, lookback, T0), tri(A, T0, I0), M = I1 / I0 - 1.

  # Candidates: index members that are liquid and have a momentum value.
  rel cand(-A: Equity, @T: Timestamp, -M: Scalar)
  cand(A, T, M) :- universe(A, T), member(A, T, "SPX"), liquid(A, T, min_adv), mom(A, T, M).

  # The n_hold strongest candidates at each rebalance date.
  rel sel(-A: Equity, @T: Timestamp)
  sel(A, T) :- mstart(T), top(n_hold, cand(A, T, M), by (M desc, A asc)).

  decide(T, target_weight(A, W)) :- sel(A, T), W = 1 / n_hold.
  decide(T, target_weight(A, 0)) :- mstart(T), held(A, T, _), not sel(A, T).
}
