# expect: N
# `close` is not complete: a missing price is unknown, not false, so
# `not close(A, T, _)` could fire on a data gap (WF-5).
strategy bad_negation_incomplete_primitive {
  env equities_1d
  uses features
  resolution @1d
  mode delta
  rel halted(-A: Equity, @T: Timestamp)
  halted(A, T) :- universe(A, T), not close(A, T, _).
  decide(T, sell(A, Q)) :- halted(A, T), held(A, T, Q).
}
