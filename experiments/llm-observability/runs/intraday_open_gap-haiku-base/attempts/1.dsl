strategy intraday_open_gap {
  env equities_1m
  resolution @1m
  mode delta
  param gap : Scalar = 0.01 in 0.0..0.2
  param qty : Quantity<Shares> = 100 shares
  rel gap_open(-A: Equity, @T: Timestamp, -G: Scalar)
  gap_open(A, T, G) :- universe_m(A, T), close_m(A, T, P1), day_start(T),
      prev(T, T0), close_m(A, T0, P0), G = P1 / P0 - 1.
  decide(T, buy(A, qty)) :- gap_open(A, T, G), G > gap.
  decide(T, sell(A, Q, moc)) :- universe_m(A, T), prev(T, T1),
      decided(T1, buy(A, _)), position(A, T, Q), Q > 0 shares.
}
