# PROBE, not a revision of r8l: does a minute rule's as-of read of a daily relation see the
# bar of the day it is in? It buys one contract at each decision time where the daily bar's
# close before minute 30 equals today's minute print. Under the documented rule (a daily bar
# counts only once its day has ended) this never happens for the day itself.
strategy r8l_probe {
  env futures_sessions
  resolution @1m
  mode delta

  rel c30d(+A: Equity, @T: Timestamp, -C: Price<USD>) @1d
  c30d(A, T, C) :- resample(close30_m(A, T1, P) to @1d as T, min 1, C = last(P)).

  decide(T, buy(A, 1 shares)) :- clock(A, T, S),
      close30_m(A, T2, X) asof T, S < T2,
      c30d(A, T3, Y) asof T, Y = X.
}
