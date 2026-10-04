# expect: E
# `open` is a primitive of equities_1d_ext, not of the declared equities_1d.
strategy bad_tier2_in_tier1 {
  env equities_1d
  uses features
  resolution @1d
  mode delta
  param qty : Quantity<Shares> = 100 shares
  rel gap(-A: Equity, @T: Timestamp)
  gap(A, T) :- universe(A, T), open(A, T, O), prev(T, T0), close(A, T0, C), O < C.
  decide(T, buy(A, qty)) :- gap(A, T), flat(A, T).
}
