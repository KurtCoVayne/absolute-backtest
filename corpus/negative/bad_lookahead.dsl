# expect: F
# `close(A, T1, P1)` binds its temporal key to a fresh T1 that is not derived
# from T; the comparison T1 > T afterwards makes it a forward return (WF-6).
strategy bad_lookahead {
  env equities_1d
  uses features
  resolution @1d
  mode delta
  param qty : Quantity<Shares> = 100 shares
  rel fwd(-A: Equity, @T: Timestamp, -R: Scalar)
  fwd(A, T, R) :- universe(A, T), close(A, T, P0), close(A, T1, P1), T1 > T, R = P1 / P0 - 1.
  decide(T, buy(A, qty)) :- fwd(A, T, R), R > 0.02, flat(A, T).
}
