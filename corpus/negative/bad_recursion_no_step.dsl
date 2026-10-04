# expect: R
# `trend` refers to itself at the same T: a cycle with no strictly earlier
# step through prev or lag (WF-4).
strategy bad_recursion_no_step {
  env equities_1d
  uses features
  resolution @1d
  mode delta
  param qty : Quantity<Shares> = 100 shares
  rel trend(-A: Equity, @T: Timestamp)
  trend(A, T) :- universe(A, T), close(A, T, P), sma(A, T, 50d, 30, M), P > M.
  trend(A, T) :- universe(A, T), trend(A, T).
  decide(T, buy(A, qty)) :- trend(A, T), flat(A, T).
}
