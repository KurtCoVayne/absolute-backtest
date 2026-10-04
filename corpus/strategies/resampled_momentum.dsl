# Daily momentum top-n computed from minute bars resampled to @1d: the
# positive cross-resolution case (section 8).
strategy resampled_momentum {
  env equities_1m
  uses bars
  resolution @1d
  mode target

  param lookback : Duration = 20d in 10d..120d
  param n : Count = 2

  rel mom(-A: Equity, @T: Timestamp, -M: Scalar)
  mom(A, T, M) :- universe_d(A, T), close_d(A, T, P1), lag(T, lookback, T0), close_d(A, T0, P0), M = P1 / P0 - 1.

  rel chosen(-A: Equity, @T: Timestamp)
  chosen(A, T) :- bar_d(T), top(n, mom(A, T, M), by (M desc, A asc)).

  rel n_chosen(@T: Timestamp, -N: Count)
  n_chosen(T, N) :- bar_d(T), N = count(A) over (chosen(A, T)), N > 0.

  decide(T, target_weight(A, W)) :- chosen(A, T), n_chosen(T, N), W = 1 / N.
  decide(T, target_weight(A, 0)) :- position(A, T, _), not chosen(A, T).
}
