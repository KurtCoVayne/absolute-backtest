# Opening-gap strategy on minute bars (equities_1m). A bar is labelled by its
# close instant, so the session's first minute is 09:31 and its last is 16:00.
#   Entry: if the first minute's close is more than min_gap above the previous
#          close (the previous bar, which is the last minute of the previous
#          session), buy qty at the next bar: the second minute, a market order
#          that fills at that bar's close.
#   Exit:  at the second minute, sell the shares bought one bar earlier; a `moc`
#          order fills at the close of the session's last minute.
strategy intraday_open_gap {
  env equities_1m
  resolution @1m
  mode delta
  # The one-percent threshold and the order size; the brief fixes neither
  # (the size is an assumption, 100 shares).
  param min_gap : Scalar = 0.01 in 0.0..0.2
  param qty : Quantity<Shares> = 100 shares

  # The first minute of each session: its close relative to the previous bar's close.
  rel gap_open(-A: Equity, @T: Timestamp, -G: Scalar)
  gap_open(A, T, G) :- universe_m(A, T), close_m(A, T, P1), day_start(T),
      prev(T, T0), close_m(A, T0, P0), G = P1 / P0 - 1.

  # Entry: decided at the first minute, so it fills at the second minute's close.
  decide(T, buy(A, qty)) :- gap_open(A, T, G), G > min_gap.

  # Exit: at the bar after an entry decision (the second minute), sell what is held.
  # The `moc` order fills at the close of the session's last minute.
  decide(T, sell(A, Q, moc)) :- universe_m(A, T), prev(T, T1),
      decided(T1, buy(A, _)), position(A, T, Q), Q > 0 shares.
}
