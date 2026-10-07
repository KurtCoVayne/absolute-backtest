strategy open_gap {
  env equities_1m
  uses bars
  uses features_m
  resolution @1m
  mode delta
  param gap : Scalar = 0.01
  param qty : Quantity<Shares> = 100 shares

  rel gapped(-A: Equity, @T: Timestamp)
  gapped(A, T) :- universe_m(A, T), day_start(T), close_m(A, T, P),
      close_d(A, T0, C) asof T, P > C * (1 + gap).

  decide(T, buy(A, qty)) :- gapped(A, T).
  decide(T, sell(A, Q, moc)) :- held_m(A, T, Q).
}
