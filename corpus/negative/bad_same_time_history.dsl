# expect: F
# `decided` must be read strictly before T; at the rule's own T the decision
# would see itself (WF-6 strictness, WF-9).
strategy bad_same_time_history {
  env equities_1d
  uses features
  resolution @1d
  mode delta
  param qty : Quantity<Shares> = 100 shares
  rel strong(-A: Equity, @T: Timestamp)
  strong(A, T) :- universe(A, T), momentum(A, T, 3mo, 0d, M), M > 0.1.
  decide(T, buy(A, qty)) :- strong(A, T), flat(A, T), not decided(T, buy(A, _)).
}
