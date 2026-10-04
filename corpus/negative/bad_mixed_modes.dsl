# expect: C
# The strategy declares delta mode but a decide rule uses a target constructor (WF-9).
strategy bad_mixed_modes {
  env equities_1d
  uses features
  resolution @1d
  mode delta
  param qty : Quantity<Shares> = 100 shares
  rel strong(-A: Equity, @T: Timestamp)
  strong(A, T) :- universe(A, T), momentum(A, T, 3mo, 0d, M), M > 0.1.
  decide(T, buy(A, qty)) :- strong(A, T), flat(A, T).
  decide(T, target_weight(A, 0)) :- held(A, T, _), strong(A, T).
}
