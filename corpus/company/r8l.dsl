# MORNIGHT-R8L: the intraday futures opening-range book (d20-research PB-5
# amendment 2026-09-02, ledger row 150; scripts/r31_return.py `load`,
# scripts/r37_book.py `build_basic` with latefrac_max 1/3, no_stop, no_late;
# scripts/r39_fixed.py). Data: scripts/ingest/tradestation_sessions.py -> env
# futures_sessions, universe HOME-26.
#
# Run as the book runs (fixed $1M, f = 0.6494% of it a leg per ATR, commission
# per contract only):
#   abt run --strategy r8l --bundle B --price-relation close_m --compounding off \
#     --capital 1000000 --lot fractional --frictionless --margin-rate 0 \
#     --on-leverage allow --report-by day --report-calendar calendar \
#     --periods-per-year 252 corpus/env corpus/company/r8l.dsl
#
# Rules, per root and session (minute k = the k-th minute of the primary session):
#   ATR      the mean true range of the root's last 20 sessions, as of the
#            previous session
#   m        (close before minute 30 - open of minute 0) / ATR
#   g        (open of minute 0 - previous session's close) / ATR
#   m_med    the median |m| over the root's last 250 sessions (at least 100
#            with a value), as of the previous session
#   main     |m| >= 2 m_med, the gap agrees (|g| < 1 or g and m share a sign)
#            and the morning is settled ((close before 30 - close before 20)
#            over m ATR <= 1/3): with m's sign
#   sidecar  |m| < 2 m_med and g <= -0.75: short
#   leg      decided 30 minutes after the open, entered at the next print's
#            open and closed at the session's last print (moo_moc), for
#            dollars / (ATR x multiplier) contracts, fractional
#
# Known difference from the Python book: where minute 30 did not trade, the
# entry is the next traded minute's open, where the book fills at the close
# before minute 30 (231 of 21,892 legs); a session with no print after minute
# 29 makes no leg here, a commission-only leg there.
strategy r8l {
  env futures_sessions
  resolution @1m
  mode target

  param atr_n : Count = 20
  param med_n : Count = 250
  param med_min : Count = 100
  param k_main : Scalar = 2
  param gap_free : Scalar = 1
  param one : Scalar = 1
  param short : Scalar = -1
  param three : Scalar = 3
  param side_gap : Scalar = -0.75
  param no_price : Price<USD> = 0 USD/share
  param dollars : Notional<USD> = 6493.912071090746 USD

  # Daily features over each root's own sessions (a root's first session has
  # no true range: it has no previous close).
  rel prev_close(+A: Equity, @T: Timestamp, -P: Price<USD>) @1d
  prev_close(A, T, P) :- session(A, T), prev(T, T1), close_d(A, _, P) asof T1.
  rel trange(+A: Equity, @T: Timestamp, -V: Price<USD>) @1d
  trange(A, T, V) :- high_d(A, T, H), low_d(A, T, L), prev_close(A, T, C), V = greatest(H - L, abs(H - C), abs(L - C)).
  rel atr(+A: Equity, @T: Timestamp, -V: Price<USD>) @1d
  atr(A, T, V) :- session(A, T), V = mean(R) over (T1 in rows(T, atr_n, min atr_n), session(A, T1), trange(A, T1, R)).

  # The session's m, for the median of the sessions before.
  rel day_m(+A: Equity, @T: Timestamp, -M: Scalar) @1d
  day_m(A, T, M) :- session(A, T),
      resample(open0_m(A, T0, O) to @1d as T, min 1, O0 = first(O)),
      resample(close30_m(A, T3, C) to @1d as T, min 1, C30 = last(C)),
      prev(T, T1), atr(A, _, V) asof T1, V > no_price,
      M = (C30 - O0) / V.
  rel abs_m(+A: Equity, @T: Timestamp, -V: Scalar) @1d
  abs_m(A, T, V) :- day_m(A, T, M), V = abs(M).
  rel m_med(+A: Equity, @T: Timestamp, -Q: Scalar) @1d
  m_med(A, T, Q) :- session(A, T), Q = median(V) over (T1 in rows(T, med_n, min med_min), session(A, T1), abs_m(A, T1, V)).

  # At the decision time: today's prints, and the daily features of the
  # previous session (a daily bar is there once its day has closed; a feature
  # the previous session lacks is missing, not carried from an earlier one).
  rel sig(-A: Equity, @T: Timestamp, -M: Scalar, -G: Scalar, -V: Price<USD>, -Q: Scalar, -O: Price<USD>, -C20: Price<USD>)
  sig(A, T, M, G, V, Q, O, C20) :- clock(A, T, S),
      open0_m(A, T0, O) asof T, T0 > S,
      close30_m(A, T3, C30) asof T, T3 > S,
      close20_m(A, T2, C20) asof T, T2 > S,
      close_d(A, D, PC) asof T,
      atr(A, D1, V) asof T, D1 = D, V > no_price,
      m_med(A, D2, Q) asof T, D2 = D,
      M = (C30 - O) / V, G = (O - PC) / V.

  rel aligned(+A: Equity, @T: Timestamp)
  aligned(A, T) :- sig(A, T, _, G, _, _, _, _), abs(G) < gap_free.
  aligned(A, T) :- sig(A, T, M, G, _, _, _, _), G * M > 0.

  # The legs: direction and ATR.
  rel leg(-A: Equity, @T: Timestamp, -Dir: Scalar, -V: Price<USD>)
  leg(A, T, one, V) :- sig(A, T, M, _, V, Q, O, C20), M > 0, abs(M) >= k_main * Q, aligned(A, T),
      (O + M * V - C20) / (M * V) <= one / three.
  leg(A, T, short, V) :- sig(A, T, M, _, V, Q, O, C20), M < 0, abs(M) >= k_main * Q, aligned(A, T),
      (O + M * V - C20) / (M * V) <= one / three.
  leg(A, T, short, V) :- sig(A, T, M, G, V, Q, _, _), abs(M) < k_main * Q, G <= side_gap.

  decide(T, target_quantity(A, N, moo_moc)) :- leg(A, T, Dir, V), multiplier(A, _, MU) asof T, N = Dir * dollars / (V * MU).
}
