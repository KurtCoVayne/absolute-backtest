# Equal-weight top-n momentum, rebalanced on the first bar of each month.
strategy monthly_rebalance {
  env equities_1d
  uses features
  resolution @1d
  mode target

  param lookback : Duration = 6mo in 3mo..1y
  param skip : Duration = 1mo
  param n : Count = 3

  rel rebalance(@T: Timestamp)
  rebalance(T) :- bar(T), month_start(T).

  rel candidate(-A: Equity, @T: Timestamp, -M: Scalar)
  candidate(A, T, M) :- rebalance(T), universe(A, T), momentum(A, T, lookback, skip, M).

  rel chosen(-A: Equity, @T: Timestamp)
  chosen(A, T) :- rebalance(T), top(n, candidate(A, T, M), by (M desc, A asc)).

  rel n_chosen(@T: Timestamp, -N: Count)
  n_chosen(T, N) :- rebalance(T), N = count(A) over (chosen(A, T)), N > 0.

  decide(T, target_weight(A, W)) :- chosen(A, T), n_chosen(T, N), W = 1 / N.
  decide(T, target_weight(A, 0)) :- rebalance(T), held(A, T, _), not chosen(A, T).
}
