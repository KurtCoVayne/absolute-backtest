# Minute-resolution executor views for strategies that decide at @1m.
library features_m {
  env equities_1m
  resolution @1m

  rel held_m(-A: Equity, @T: Timestamp, -Q: Quantity<Shares>)
  held_m(A, T, Q) :- position(A, T, Q), Q > 0 shares.

  rel flat_m(+A: Equity, @T: Timestamp)
  flat_m(A, T) :- universe_m(A, T), not position(A, T, _).
}
