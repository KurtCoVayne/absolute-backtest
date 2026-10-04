# expect: X
# `features_m` (@1m) reads `position`, which the kernel supplies at the
# strategy's decision resolution (section 6); under a strategy deciding at
# @5m that is @5m, so the library's rules mix resolutions (WF-10). A library
# that reads executor relations is usable only by strategies deciding at its
# own resolution.
strategy bad_lib_executor_resolution {
  env equities_1m
  uses features_m
  resolution @5m
  mode delta
  param qty : Quantity<Shares> = 100 shares
  rel universe_5(-A: Equity, @T: Timestamp)
  universe_5(A, T) :- resample(universe_m(A, T1) to @5m as T, min 1, N = count(T1)).
  decide(T, buy(A, qty)) :- universe_5(A, T), flat_m(A, T).
}
