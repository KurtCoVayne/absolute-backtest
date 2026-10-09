# Breakout with a trailing stop, attempt 1.
strategy breakout_with_stop {
  env equities_1d
  uses features
  resolution @1d
  mode target

  param lookback : Duration = 52w in 26w..104w
  param min_obs : Count = 200 in 100..250
  param avg_days : Count = 20 in 10..60
  param vol_mult : Scalar = 2.0 in 1.0..5.0
  param max_drop : Scalar = 0.08 in 0.02..0.30
  param max_names : Count = 10 in 1..50

  # Mean daily volume over the avg_days sessions before T (T itself excluded).
  rel vol_avg(-A: Equity, @T: Timestamp, -AV: Quantity<Shares>)
  vol_avg(A, T, AV) :- universe(A, T), prev(T, T0),
      AV = mean(V) over (T1 in rows(T0, avg_days, min avg_days), volume(A, T1, V)).

  # A new 52-week closing high (above every prior close in the window) on
  # volume above vol_mult times the average of the prior avg_days sessions.
  rel breakout(-A: Equity, @T: Timestamp, -RV: Scalar)
  breakout(A, T, RV) :- universe(A, T), highest(A, T, lookback, min_obs, H),
      close(A, T, P), P > H,
      volume(A, T, V), vol_avg(A, T, AV), V > vol_mult * AV, RV = V / AV.

  # A breakout in a name that is not held at T.
  rel cand(-A: Equity, @T: Timestamp, -RV: Scalar)
  cand(A, T, RV) :- breakout(A, T, RV), not held(A, T, _).

  # Free slots at T: max_names less the names held at T.
  rel slots(@T: Timestamp, -S: Count)
  slots(T, S) :- bar(T), C = count(A1) over (held(A1, T, _)), S = max_names - C.

  # The strongest candidates that fit the free slots: volume ratio, then ticker order.
  rel pick(-A: Equity, @T: Timestamp)
  pick(A, T) :- bar(T), slots(T, S), top(S, cand(A, T, RV), by (RV desc, A asc)).

  # Held at the bar before T.
  rel held_before(-A: Equity, @T: Timestamp)
  held_before(A, T) :- prev(T, T0), held(A, T0, _).

  # Highest close since entry, carried forward while the name is held.
  rel trail_hi(-A: Equity, @T: Timestamp, -H: Price<USD>)
  trail_hi(A, T, P) :- held(A, T, _), close(A, T, P), not held_before(A, T).
  trail_hi(A, T, H) :- held(A, T, _), prev(T, T0), trail_hi(A, T0, H0),
      close(A, T, P), H = greatest(H0, P).

  decide(T, target_weight(A, W)) :- pick(A, T), W = 1 / max_names.
  decide(T, target_weight(A, 0)) :- held(A, T, _), trail_hi(A, T, H), close(A, T, P),
      P < H * (1 - max_drop).
}
