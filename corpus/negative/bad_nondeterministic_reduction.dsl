# expect: D
# `first` over an arbitrary group has no order; it is deterministic only
# inside a resample, where the fine temporal key orders it (WF-7).
strategy bad_nondeterministic_reduction {
  env equities_1d
  uses features
  resolution @1d
  mode delta
  param qty : Quantity<Shares> = 100 shares
  rel ref_price(-A: Equity, @T: Timestamp, -X: Price<USD>)
  ref_price(A, T, X) :- universe(A, T), X = first(P) over (T1 in window(T, 5d, min 3), close(A, T1, P)).
  decide(T, buy(A, qty)) :- ref_price(A, T, X), close(A, T, P), P > X, flat(A, T).
}
