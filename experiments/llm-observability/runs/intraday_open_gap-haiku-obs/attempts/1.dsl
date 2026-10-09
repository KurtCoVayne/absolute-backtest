# Opening-gap strategy on minute bars (attempt 1).
strategy intraday_open_gap {
  env equities_1m
  resolution @1m
  mode delta
  param gap : Scalar = 0.01
  param qty : Quantity<Shares> = 100 shares

  # The first minute bar of each calendar day.
  rel first_bar(-A: Equity, @T: Timestamp)
  first_bar(A, T) :- universe_m(A, T), day_start(T).

  # The first minute's close is more than `gap` above the previous bar's close,
  # which is the last minute of the previous day.
  rel gap_up(-A: Equity, @T: Timestamp)
  gap_up(A, T) :- first_bar(A, T), prev(T, T0), close_m(A, T0, P0), close_m(A, T, P1),
      R = P1 / P0 - 1, R > gap.

  # The second minute bar of a day whose first minute gapped up.
  rel second_after_gap(-A: Equity, @T: Timestamp)
  second_after_gap(A, T) :- universe_m(A, T), prev(T, T0), gap_up(A, T0).

  # Buy at the first minute's decision, filled at the next bar (the second minute).
  decide(T, buy(A, qty)) :- gap_up(A, T).
  # Sell the held shares at the second minute, market-on-close: filled at the day's last minute.
  decide(T, sell(A, Q, moc)) :- second_after_gap(A, T), position(A, T, Q), Q > 0 shares.
}
