# Daily feature library, written in the DSL itself (section 1). Every window
# names its minimum observation count, so each windowed feature takes the
# count as an input next to the duration.
library features {
  env equities_1d
  resolution @1d

  # Every bar of the daily time domain (section 6).
  rel bar(@T: Timestamp)
  bar(T) :- universe(_, T).

  rel sma(+A: Equity, @T: Timestamp, +N: Duration, +K: Count, -M: Price<USD>)
  sma(A, T, N, K, M) :- universe(A, T),
      M = mean(P) over (T1 in window(T, N, min K), close(A, T1, P)).

  rel logret(+A: Equity, @T: Timestamp, -R: Scalar)
  logret(A, T, R) :- close(A, T, P), prev(T, T0), close(A, T0, P0), R = log(P / P0).

  rel realized_vol(+A: Equity, @T: Timestamp, +N: Duration, +K: Count, -V: Scalar)
  realized_vol(A, T, N, K, V) :- universe(A, T),
      V = std(R) over (T1 in window(T, N, min K), logret(A, T1, R)).

  rel zscore(+A: Equity, @T: Timestamp, +N: Duration, +K: Count, -Z: Scalar)
  zscore(A, T, N, K, Z) :- close(A, T, P),
      M = mean(P1) over (T1 in window(T, N, min K), close(A, T1, P1)),
      S = std(P1) over (T1 in window(T, N, min K), close(A, T1, P1)),
      Z = (P - M) / S.

  # Return from Lookback ago to Skip ago; Skip = 0d means "to the latest close".
  rel momentum(+A: Equity, @T: Timestamp, +Lookback: Duration, +Skip: Duration, -M: Scalar)
  momentum(A, T, Lookback, Skip, M) :- universe(A, T),
      lag(T, Skip, T1), close(A, T1, P1),
      lag(T, Lookback, T0), close(A, T0, P0),
      M = P1 / P0 - 1.

  # Average daily dollar volume: Price * Quantity = Notional by dimension.
  rel adv(+A: Equity, @T: Timestamp, +N: Duration, +K: Count, -D: Notional<USD>)
  adv(A, T, N, K, D) :- universe(A, T),
      D = mean(P * V) over (T1 in window(T, N, min K), close(A, T1, P), volume(A, T1, V)).

  rel mom_candidate(+A: Equity, @T: Timestamp, +Lookback: Duration, +Skip: Duration, +MinAdv: Notional<USD>, -M: Scalar)
  mom_candidate(A, T, Lookback, Skip, MinAdv, M) :- universe(A, T),
      momentum(A, T, Lookback, Skip, M), adv(A, T, 20d, 10, D), D >= MinAdv.

  rel highest(+A: Equity, @T: Timestamp, +N: Duration, +K: Count, -H: Price<USD>)
  highest(A, T, N, K, H) :- universe(A, T),
      H = max(P) over (T1 in prior_window(T, N, min K), close(A, T1, P)).

  # Executor feedback views. Both are complete (WF-5): position is kernel-supplied.
  rel held(-A: Equity, @T: Timestamp, -Q: Quantity<Shares>)
  held(A, T, Q) :- position(A, T, Q), Q > 0 shares.

  rel flat(+A: Equity, @T: Timestamp)
  flat(A, T) :- universe(A, T), not position(A, T, _).

  # A buy was decided for A within the last N (calendar) of history; complete,
  # so `not bought_within(...)` is a legal cooldown.
  rel bought_within(+A: Equity, @T: Timestamp, +N: Duration)
  bought_within(A, T, N) :- universe(A, T),
      C = count(T0) over (T0 in prior_window(T, N, min 1), decided(T0, buy(A, _))), C > 0.
}
