# Fade a gap down at the open. Not expressible at @1d (section 6, availability
# convention): the strategy decides at @1m, where the first bar of the day is
# available at its own close and the prior close is the bar before it.
strategy opening_gap {
  env equities_1m
  uses features_m
  resolution @1m
  mode delta

  param gap : Scalar = 0.02 in 0.005..0.1
  param qty : Quantity<Shares> = 100 shares

  rel gap_down(-A: Equity, @T: Timestamp, -G: Scalar)
  gap_down(A, T, G) :- universe_m(A, T), day_start(T), close_m(A, T, P),
      prev(T, T0), close_m(A, T0, P0), G = P / P0 - 1, G < -gap.

  decide(T, buy(A, qty)) :- gap_down(A, T, _), flat_m(A, T).
  decide(T, sell(A, Q)) :- held_m(A, T, Q), day_start(T).
}
