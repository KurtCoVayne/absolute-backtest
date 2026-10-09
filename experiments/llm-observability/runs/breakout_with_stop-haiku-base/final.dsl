# breakout_with_stop: buy a name when its close is a new 52-week closing high
# (above every close of the prior 52 weeks, which must be fully on record) and
# its volume is above twice its average over the 20 bars before today. Sell when
# the close falls more than 8% below the highest close since entry. One position
# per name, at most max_names names, each a target weight of 1 / max_names.
# Decisions made at the close of T fill at the close of the next bar.
strategy breakout_with_stop {
  env equities_1d
  uses features
  resolution @1d
  mode target

  # Length of the high window.
  param lookback : Duration = 52w in 26w..104w
  # Closes required inside the high window.
  param hi_min : Count = 240 in 120..260
  # Bars in the volume average.
  param vol_days : Count = 20 in 10..60
  # Volume must exceed this multiple of its average.
  param vol_mult : Scalar = 2 in 1..5
  # Fall from the highest close since entry that triggers a sell.
  param stop_drop : Scalar = 0.08 in 0.02..0.3
  # Names held at most, each with an equal target weight.
  param max_names : Count = 10 in 1..20

  # Average volume over the vol_days bars before T.
  rel avg_vol(-A: Equity, @T: Timestamp, -M: Quantity<Shares>)
  avg_vol(A, T, M) :- universe(A, T), prev(T, T0),
      M = mean(V) over (T1 in rows(T0, vol_days, min vol_days), volume(A, T1, V)).

  # Breakout candidates: flat, a full history, a new high of closes over the
  # prior window, and a volume surge. R is the volume ratio, used to rank them.
  rel cand(-A: Equity, @T: Timestamp, -R: Scalar)
  cand(A, T, R) :- universe(A, T), not held(A, T, _), lag(T, lookback, _),
      close(A, T, P), highest(A, T, lookback, hi_min, H), P > H,
      volume(A, T, V), avg_vol(A, T, M), V > vol_mult * M, R = V / M.

  # Free slots on T: max_names less the names held at T.
  rel open_slots(@T: Timestamp, -N: Count)
  open_slots(T, N) :- universe(_, T), C = count(A1) over (held(A1, T, _)), N = max_names - C.

  # The strongest candidates on T, as many as there are free slots.
  rel picked(-A: Equity, @T: Timestamp)
  picked(A, T) :- open_slots(T, N), top(N, cand(A, T, R), by (R desc, A asc)).

  # Highest close since entry for a held name. Entry is the first held bar.
  rel peak(-A: Equity, @T: Timestamp, -H: Price<USD>)
  peak(A, T, P) :- held(A, T, _), close(A, T, P), prev(T, T0), not held(A, T0, _).
  peak(A, T, H) :- held(A, T, _), close(A, T, P), prev(T, T0), peak(A, T0, H0),
      H = greatest(H0, P).

  decide(T, target_weight(A, W)) :- picked(A, T), W = 1 / max_names.
  decide(T, target_weight(A, 0)) :- held(A, T, _), peak(A, T, H), close(A, T, P),
      D = 1 - P / H, D > stop_drop.
}
