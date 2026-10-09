# momentum_liquid_monthly: the ten strongest 12-1 month total-return index
# members that clear a $1m ADV floor, equally weighted, rebalanced at each
# month start; everything else flat.
strategy momentum_liquid_monthly {
  env equities_1d_v2
  uses catalog
  resolution @1d
  mode target

  param hold : Count = 10
  param min_adv : Notional<USD> = 1_000_000 USD
  param look : Duration = 12mo
  param skip : Duration = 1mo
  # Bars required in the look-back window (weekday bars: about 11.5 months).
  param min_obs : Count = 240
  param index : Label = "SPX"

  # The first bar of each calendar month: the rebalance dates.
  rel mstart(@T: Timestamp)
  mstart(T) :- bar(T), month_start(T).

  # Sum of the total log returns (catalog logret, dividends included) over
  # the window of length N ending at T.
  rel logsum(+A: Equity, @T: Timestamp, +N: Duration, +K: Count, -S: Scalar)
  logsum(A, T, N, K, S) :- universe(A, T),
      S = sum(R) over (T1 in window(T, N, min K), logret(A, T1, R)).

  # 12-1 momentum: the log total return from T - look to T - skip, i.e. the
  # look-back sum less the most recent month's sum. The lag gate requires the
  # data to reach back a full look before T.
  rel mom(+A: Equity, @T: Timestamp, -M: Scalar)
  mom(A, T, M) :- universe(A, T), lag(T, look, _),
      logsum(A, T, look, min_obs, S1), logsum(A, T, skip, 1, S2), M = S1 - S2.

  # Index members on T with enough liquidity (catalog liquid: 20-day average
  # of close x volume at least min_adv) and a momentum value.
  rel cand(-A: Equity, @T: Timestamp, -M: Scalar)
  cand(A, T, M) :- universe(A, T), member(A, T, index), liquid(A, T, min_adv), mom(A, T, M).

  # The `hold` strongest candidates on each rebalance date.
  rel selected(-A: Equity, @T: Timestamp)
  selected(A, T) :- mstart(T), top(hold, cand(A, T, M), by (M desc, A asc)).

  # Equal weight for the selected names. Target mode does not liquidate on its
  # own, so a held name that is no longer selected gets an explicit target of 0.
  decide(T, target_weight(A, W)) :- mstart(T), selected(A, T), W = 1 / hold.
  decide(T, target_weight(A, 0)) :- mstart(T), held(A, T, _), not selected(A, T).
}
