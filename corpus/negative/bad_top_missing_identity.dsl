# expect: D
# The order keys omit the identity column A: two candidates with equal
# momentum would tie, and the checker does not accept an order that is total
# only because values happen to be distinct (WF-7).
strategy bad_top_missing_identity {
  env equities_1d
  uses features
  resolution @1d
  mode target
  param n : Count = 3
  rel candidate(-A: Equity, @T: Timestamp, -M: Scalar)
  candidate(A, T, M) :- universe(A, T), momentum(A, T, 6mo, 1mo, M).
  rel selected(-A: Equity, @T: Timestamp)
  selected(A, T) :- bar(T), top(n, candidate(A, T, M), by (M desc)).
  decide(T, target_weight(A, 0.1)) :- selected(A, T).
}
