strategy intraday_open_gap {
  env equities_1m
  uses features_m, bars
  resolution @1m
  mode delta

  # Identify first minute of day with gap > 1%
  rel gap_up_day(+A: Equity, @T: Timestamp)
  gap_up_day(A, T) :-
    universe_m(A, T),
    day_start(T),
    close_m(A, T, P1),
    close_d(A, T0, P0) asof T,
    P1 > P0 * 1.01.

  # Buy at the second minute if gap is up
  decide(T, buy(A, 100 shares)) :-
    prev(T, T0),
    gap_up_day(A, T0),
    flat_m(A, T).

  # Sell at market-on-close during the day
  decide(T, sell(A, Q, moc)) :-
    held_m(A, T, Q),
    not day_start(T).
}
