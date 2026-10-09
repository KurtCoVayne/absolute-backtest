# PROBE CONTROL, not a revision of r8l: the same as attempt 4 without the equality test. It
# buys one contract wherever the daily as-of read finds a daily bar at all, so a zero count in
# attempt 4 can only mean the equality failed, not that the read is broken.
strategy r8l_probe_ctl {
  env futures_sessions
  resolution @1m
  mode delta

  rel c30d(+A: Equity, @T: Timestamp, -C: Price<USD>) @1d
  c30d(A, T, C) :- resample(close30_m(A, T1, P) to @1d as T, min 1, C = last(P)).

  decide(T, buy(A, 1 shares)) :- clock(A, T, S),
      close30_m(A, T2, _) asof T, S < T2,
      c30d(A, T3, _) asof T.
}
