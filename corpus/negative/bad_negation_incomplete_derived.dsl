# expect: N
# `selected` includes a benchmark-dependent condition, so it is not complete;
# `not selected(A, T)` would let a missing benchmark close liquidate the book.
strategy bad_negation_incomplete_derived {
  env equities_1d
  uses features
  resolution @1d
  mode target
  param benchmark : Equity = "SPY"
  param n : Count = 3
  rel regime_on(@T: Timestamp)
  regime_on(T) :- close(benchmark, T, P), sma(benchmark, T, 200d, 120, M), P > M.
  rel candidate(-A: Equity, @T: Timestamp, -M: Scalar)
  candidate(A, T, M) :- universe(A, T), momentum(A, T, 6mo, 1mo, M).
  rel in_top(-A: Equity, @T: Timestamp)
  in_top(A, T) :- bar(T), top(n, candidate(A, T, M), by (M desc, A asc)).
  rel selected(-A: Equity, @T: Timestamp)
  selected(A, T) :- regime_on(T), in_top(A, T).
  rel n_selected(@T: Timestamp, -N: Count)
  n_selected(T, N) :- regime_on(T), N = count(A) over (in_top(A, T)), N > 0.
  decide(T, target_weight(A, W)) :- selected(A, T), n_selected(T, N), W = 1 / N.
  decide(T, target_weight(A, 0)) :- held(A, T, _), not selected(A, T).
}
