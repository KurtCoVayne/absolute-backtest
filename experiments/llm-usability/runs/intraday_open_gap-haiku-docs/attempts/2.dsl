strategy intraday_open_gap {
  env equities_1m
  uses features_m, bars
  resolution @1m
  mode delta

  param gap_threshold : Scalar = 0.01 in 0.001..0.05
  param buy_qty : Quantity<Shares> = 100 shares

  # Identify first minute of day with gap > threshold
  rel gap_up_day(+A: Equity, @T: Timestamp)
  gap_up_day(A, T) :-
    universe_m(A, T),
    day_start(T),
    close_m(A, T, P1),
    close_d(A, T0, P0) asof T,
    P1 > P0 * (1 + gap_threshold).

  # Derived relation: not the first minute of the day
  rel not_first_min(@T: Timestamp)
  not_first_min(T) :-
    universe_m(_, T),
    not day_start(T).

  # Buy at the second minute if gap is up
  decide(T, buy(A, buy_qty)) :-
    universe_m(A, T),
    prev(T, T0),
    gap_up_day(A, T0),
    flat_m(A, T).

  # Sell at market-on-close during the day
  decide(T, sell(A, Q, moc)) :-
    held_m(A, T, Q),
    not_first_min(T).
}
