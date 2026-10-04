# expect: Z
# A strategy with rules but no decide rule produces nothing (WF-9).
strategy bad_no_decision {
  env equities_1d
  uses features
  resolution @1d
  mode delta
  rel strong(-A: Equity, @T: Timestamp)
  strong(A, T) :- universe(A, T), momentum(A, T, 3mo, 0d, M), M > 0.1.
}
