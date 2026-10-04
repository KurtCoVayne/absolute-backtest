# expect: S
# `a` and `b` negate each other: a cycle through negation has no stratification (WF-8).
strategy bad_negation_cycle {
  env equities_1d
  uses features
  resolution @1d
  mode delta
  param qty : Quantity<Shares> = 100 shares
  rel a(-A: Equity, @T: Timestamp)
  a(A, T) :- universe(A, T), not b(A, T).
  rel b(-A: Equity, @T: Timestamp)
  b(A, T) :- universe(A, T), not a(A, T).
  decide(T, buy(A, qty)) :- a(A, T), flat(A, T).
}
