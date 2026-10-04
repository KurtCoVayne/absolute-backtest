# Equal-weight the top-n momentum names that clear a dollar-volume floor.
# `selected` is a reduction, so it is complete and `not selected` is legal.
strategy momentum_top_n {
  env equities_1d
  uses features
  resolution @1d
  mode target

  param lookback : Duration = 1y in 6mo..2y
  param skip : Duration = 1mo in 0d..3mo
  param min_adv : Notional<USD> = 1_000_000 USD
  param n : Count = 3 in 1..50

  rel candidate(-A: Equity, @T: Timestamp, -M: Scalar)
  candidate(A, T, M) :- universe(A, T), mom_candidate(A, T, lookback, skip, min_adv, M).

  rel selected(-A: Equity, @T: Timestamp)
  selected(A, T) :- bar(T), top(n, candidate(A, T, M), by (M desc, A asc)).

  rel n_selected(@T: Timestamp, -N: Count)
  n_selected(T, N) :- bar(T), N = count(A) over (selected(A, T)), N > 0.

  decide(T, target_weight(A, W)) :- selected(A, T), n_selected(T, N), W = 1 / N.
  decide(T, target_weight(A, 0)) :- held(A, T, _), not selected(A, T).
}
