strategy intraday_open_gap {
  env equities_1m
  uses features_m, bars
  resolution @1m
  mode delta

  param gap_threshold : Scalar = 0.01 in 0.001..0.05
  param buy_qty : Quantity<Shares> = 100 shares

  # Derived relation: first minute of the day
  rel first_min(@T: Timestamp)
  first_min(T) :-
    universe_m(_, T),
    day_start(T).

  # Identify first minute of day with gap > threshold
  rel gap_up_day(+A: Equity, @T: Timestamp)
  gap_up_day(A, T) :-
    first_min(T),
    close_m(A, T, P1),
    close_d(A, T0, P0) asof T,
    P1 > P0 * (1 + gap_threshold).

  # Buy at the second minute if gap is up
  decide(T, buy(A, buy_qty)) :-
    universe_m(A, T),
    prev(T, T0),
    gap_up_day(A, T0),
    flat_m(A, T).

  # Sell at market-on-close during the day
  decide(T, sell(A, Q, moc)) :-
    held_m(A, T, Q),
    not first_min(T).
}
