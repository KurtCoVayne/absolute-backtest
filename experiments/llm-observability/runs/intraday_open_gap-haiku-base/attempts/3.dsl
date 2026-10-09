# Variant: one entry rule with the documented intraday order type moo_moc
# (in at the next bar's open, flat at the session's close).
strategy intraday_open_gap {
  env equities_1m
  resolution @1m
  mode delta
  param min_gap : Scalar = 0.01 in 0.0..0.2
  param qty : Quantity<Shares> = 100 shares
  rel gap_open(-A: Equity, @T: Timestamp, -G: Scalar)
  gap_open(A, T, G) :- universe_m(A, T), close_m(A, T, P1), day_start(T),
      prev(T, T0), close_m(A, T0, P0), G = P1 / P0 - 1.
  decide(T, buy(A, qty, moo_moc)) :- gap_open(A, T, G), G > min_gap.
}
