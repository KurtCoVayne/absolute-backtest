# Momentum on total return over the catalog environment: the top names by
# compounded total return, liquid, index members, and never a delisted one.
strategy total_return_momentum {
  env equities_1d_v2
  uses catalog
  resolution @1d
  mode target

  param lookback : Duration = 6mo in 3mo..1y
  param min_obs : Count = 60
  param min_adv : Notional<USD> = 1_000_000 USD
  param index : Label = "SPX"
  param n : Count = 2 in 1..20

  rel candidate(-A: Equity, @T: Timestamp, -M: Scalar)
  candidate(A, T, M) :- universe(A, T), member(A, T, index), liquid(A, T, min_adv),
      M = sum(R) over (T1 in window(T, lookback, min min_obs), logret(A, T1, R)).

  rel selected(-A: Equity, @T: Timestamp)
  selected(A, T) :- bar(T), top(n, candidate(A, T, M), by (M desc, A asc)).

  rel n_selected(@T: Timestamp, -N: Count)
  n_selected(T, N) :- bar(T), N = count(A) over (selected(A, T)), N > 0.

  decide(T, target_weight(A, W)) :- selected(A, T), n_selected(T, N), W = 1 / N.
  decide(T, target_weight(A, 0)) :- held(A, T, _), not selected(A, T).
}
