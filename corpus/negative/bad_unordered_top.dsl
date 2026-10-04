# expect: D
# A reduction without `by`: no total order, so which n tuples survive is
# unspecified (WF-7).
strategy bad_unordered_top {
  env equities_1d
  uses features
  resolution @1d
  mode target
  param n : Count = 3
  rel candidate(-A: Equity, @T: Timestamp, -M: Scalar)
  candidate(A, T, M) :- universe(A, T), momentum(A, T, 6mo, 1mo, M).
  rel selected(-A: Equity, @T: Timestamp)
  selected(A, T) :- bar(T), top(n, candidate(A, T, M)).
  decide(T, target_weight(A, 0.1)) :- selected(A, T).
}
