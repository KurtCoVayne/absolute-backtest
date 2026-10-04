# expect: T
# Price<USD> + Scalar: the dimensions do not balance (WF-3).
strategy bad_price_plus_scalar {
  env equities_1d
  uses features
  resolution @1d
  mode delta
  param qty : Quantity<Shares> = 100 shares
  rel bumped(-A: Equity, @T: Timestamp, -X: Price<USD>)
  bumped(A, T, X) :- universe(A, T), close(A, T, P), X = P + 0.5.
  decide(T, buy(A, qty)) :- bumped(A, T, X), close(A, T, P), X > P, flat(A, T).
}
