strategy momentum_liquid_monthly {
  env equities_1d_v2
  uses catalog
  resolution @1d
  mode target

  param lookback : Duration = 12mo
  param skip : Duration = 1mo
  param n : Count = 10
  param min_adv : Notional<USD> = 1_000_000 USD
  param idx : Label = "SP500"
  param min_obs : Count = 200
  param min_skip_obs : Count = 15
  param w : Scalar = 0.1

  rel mstart(@T: Timestamp)
  mstart(T) :- bar(T), month_start(T).

  rel mom(+A: Equity, @T: Timestamp, -M: Scalar)
  mom(A, T, M) :- universe(A, T),
      L = sum(R) over (T1 in window(T, lookback, min min_obs), logret(A, T1, R)),
      S = sum(R2) over (T2 in window(T, skip, min min_skip_obs), logret(A, T2, R2)),
      M = L - S.

  rel cand(-A: Equity, @T: Timestamp, -M: Scalar)
  cand(A, T, M) :- universe(A, T), member(A, T, idx), liquid(A, T, min_adv), mom(A, T, M).

  rel selected(-A: Equity, @T: Timestamp, -M: Scalar)
  selected(A, T, M) :- mstart(T), top(n, cand(A, T, M), by (M desc, A asc)).

  decide(T, target_weight(A, w)) :- selected(A, T, _).
  decide(T, target_weight(A, 0)) :- mstart(T), held(A, T, _), not selected(A, T, _).
}
