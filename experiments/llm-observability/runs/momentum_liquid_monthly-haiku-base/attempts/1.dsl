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
  param obs : Count = 200

  # The first bar of each calendar month: the rebalance dates.
  rel mstart(@T: Timestamp)
  mstart(T) :- bar(T), month_start(T).

  # Sum of the total log returns over the window of length N ending at T.
  rel logsum(+A: Equity, @T: Timestamp, +N: Duration, +K: Count, -S: Scalar)
  logsum(A, T, N, K, S) :- universe(A, T),
      S = sum(R) over (T1 in window(T, N, min K), logret(A, T1, R)).

  # 12-1 momentum: the log total return from T - look up to T - skip.
  rel mom(+A: Equity, @T: Timestamp, -M: Scalar)
  mom(A, T, M) :- logsum(A, T, look, obs, S1), logsum(A, T, skip, 1, S2), M = S1 - S2.

  # Index members on T with enough liquidity and a 12-1 momentum value.
  rel cand(-A: Equity, @T: Timestamp, -M: Scalar)
  cand(A, T, M) :- universe(A, T), member(A, T, "SP500"), liquid(A, T, min_adv), mom(A, T, M).

  # The `hold` strongest candidates on each rebalance date.
  rel selected(-A: Equity, @T: Timestamp)
  selected(A, T) :- mstart(T), top(hold, cand(A, T, M), by (M desc, A asc)).

  decide(T, target_weight(A, W)) :- mstart(T), selected(A, T), W = 1 / hold.
  decide(T, target_weight(A, 0)) :- mstart(T), held(A, T, _), not selected(A, T).
}
