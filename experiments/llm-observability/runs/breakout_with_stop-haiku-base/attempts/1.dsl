# breakout_with_stop, attempt 1: new 52-week closing high on volume above twice
# the prior 20-bar average; 8% trailing stop from the highest close since entry;
# equal target weights, at most ten names.
strategy breakout_with_stop {
  env equities_1d
  uses features
  resolution @1d
  mode target

  param lookback : Duration = 52w in 26w..104w
  param hi_min : Count = 200 in 100..252
  param vol_days : Count = 20 in 10..60
  param vol_mult : Scalar = 2 in 1..5
  param stop_drop : Scalar = 0.08 in 0.02..0.3
  param max_names : Count = 10 in 1..20

  # Average volume over the 20 bars before T.
  rel avg_vol(-A: Equity, @T: Timestamp, -M: Quantity<Shares>)
  avg_vol(A, T, M) :- universe(A, T), prev(T, T0),
      M = mean(V) over (T1 in rows(T0, vol_days, min vol_days), volume(A, T1, V)).

  # Breakout candidates: flat, a new high of closes over the prior window, volume surge.
  rel cand(-A: Equity, @T: Timestamp, -R: Scalar)
  cand(A, T, R) :- universe(A, T), not held(A, T, _),
      close(A, T, P), highest(A, T, lookback, hi_min, H), P > H,
      volume(A, T, V), avg_vol(A, T, M), V > vol_mult * M, R = V / M.

  rel open_slots(@T: Timestamp, -N: Count)
  open_slots(T, N) :- universe(_, T), C = count(A1) over (held(A1, T, _)), N = max_names - C.

  rel picked(-A: Equity, @T: Timestamp)
  picked(A, T) :- open_slots(T, N), top(N, cand(A, T, R), by (R desc, A asc)).

  # Highest close since entry, for a held name; entry is the first held bar.
  rel peak(-A: Equity, @T: Timestamp, -H: Price<USD>)
  peak(A, T, P) :- held(A, T, _), close(A, T, P), prev(T, T0), not held(A, T0, _).
  peak(A, T, H) :- held(A, T, _), close(A, T, P), prev(T, T0), peak(A, T0, H0),
      H = greatest(H0, P).

  decide(T, target_weight(A, W)) :- picked(A, T), W = 1 / max_names.
  decide(T, target_weight(A, 0)) :- held(A, T, _), peak(A, T, H), close(A, T, P),
      D = 1 - P / H, D > stop_drop.
}
