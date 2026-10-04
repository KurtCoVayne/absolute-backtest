# expect: B
# The head variable X is bound by nothing in the body (WF-1).
strategy bad_unbound_head {
  env equities_1d
  uses features
  resolution @1d
  mode delta
  param qty : Quantity<Shares> = 100 shares
  rel signal(-A: Equity, @T: Timestamp, -X: Scalar)
  signal(A, T, X) :- universe(A, T), logret(A, T, R), R > 0.
  decide(T, buy(A, qty)) :- signal(A, T, X), X > 0.5, flat(A, T).
}
