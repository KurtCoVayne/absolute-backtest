# expect: M
# `sma` takes +N: Duration, and N is unbound at the call (WF-2); the family
# would be infinite.
strategy bad_unbound_input {
  env equities_1d
  uses features
  resolution @1d
  mode delta
  param qty : Quantity<Shares> = 100 shares
  rel above(-A: Equity, @T: Timestamp)
  above(A, T) :- universe(A, T), close(A, T, P), sma(A, T, N, 10, M), P > M.
  decide(T, buy(A, qty)) :- above(A, T), flat(A, T).
}
