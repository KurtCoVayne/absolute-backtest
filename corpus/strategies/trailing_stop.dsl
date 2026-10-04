# Momentum entry with a trailing stop. The high-water mark since entry is a
# value carried forward one bar at a time: temporal recursion that WF-4
# accepts because the self-reference steps back through `prev`.
strategy trailing_stop {
  env equities_1d
  uses features
  resolution @1d
  mode delta

  param lookback : Duration = 3mo
  param skip : Duration = 0d
  param threshold : Scalar = 0.1
  param stop : Scalar = 0.08 in 0.02..0.2
  param qty : Quantity<Shares> = 100 shares

  rel high_since_entry(+A: Equity, @T: Timestamp, -H: Price<USD>)
  high_since_entry(A, T, H) :- fill(A, T, Q, P), Q > 0 shares, H = P.
  high_since_entry(A, T, H) :- position(A, T, Q), Q > 0 shares, not fill(A, T, _, _),
      prev(T, T0), high_since_entry(A, T0, H0), close(A, T, P), H = greatest(H0, P).

  rel strong(-A: Equity, @T: Timestamp)
  strong(A, T) :- universe(A, T), momentum(A, T, lookback, skip, M), M > threshold.

  decide(T, buy(A, qty)) :- strong(A, T), flat(A, T).
  decide(T, sell(A, Q)) :- held(A, T, Q), high_since_entry(A, T, H), close(A, T, P), P < H * (1 - stop).
}
