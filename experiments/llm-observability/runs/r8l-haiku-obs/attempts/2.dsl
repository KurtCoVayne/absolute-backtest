# R8L opening-range book, attempt 1.
# Daily features (per root, over the root's own sessions) live in a @1d
# library; the decisions are @1m and read them as of the previous session.

library r8l_daily {
  env futures_sessions
  resolution @1d

  # The root's previous session's close (none on its first session).
  rel pclose(+A: Equity, @T: Timestamp, -C: Price<USD>)
  pclose(A, T, C) :- session(A, T),
      C = sum(P) over (T1 in rows(T, 2, min 1), session(A, T1), close_d(A, T1, P), T1 < T).

  # True range of a session.
  rel tr(+A: Equity, @T: Timestamp, -R: Price<USD>)
  tr(A, T, R) :- session(A, T), high_d(A, T, H), low_d(A, T, L), pclose(A, T, C),
      X = greatest(H - L, abs(H - C)), R = greatest(X, abs(L - C)).

  # ATR at a session: the mean true range over the root's last 20 sessions,
  # all 20 required.
  rel atr(+A: Equity, @T: Timestamp, -V: Price<USD>)
  atr(A, T, V) :- session(A, T),
      V = mean(R) over (T1 in rows(T, 20, min 20), session(A, T1), tr(A, T1, R)).

  # The ATR of the root's previous session, as of T.
  rel atr_p(+A: Equity, @T: Timestamp, -V: Price<USD>)
  atr_p(A, T, V) :- session(A, T),
      V = sum(X) over (T1 in rows(T, 2, min 1), session(A, T1), atr(A, T1, X), T1 < T).

  # The session's open of minute 0 and close before minute 30.
  rel o0_d(+A: Equity, @T: Timestamp, -O: Price<USD>)
  o0_d(A, T, O) :- resample(open0_m(A, T1, P) to @1d as T, min 1, O = first(P)).

  rel c30_d(+A: Equity, @T: Timestamp, -C: Price<USD>)
  c30_d(A, T, C) :- resample(close30_m(A, T1, P) to @1d as T, min 1, C = last(P)).

  # m of a session: (close before minute 30 - open of minute 0) / previous ATR.
  rel m_d(+A: Equity, @T: Timestamp, -M: Scalar)
  m_d(A, T, M) :- c30_d(A, T, C), o0_d(A, T, O), atr_p(A, T, ATR), ATR > 0 USD/share,
      M = (C - O) / ATR.

  rel am_d(+A: Equity, @T: Timestamp, -X: Scalar)
  am_d(A, T, X) :- m_d(A, T, M), X = abs(M).

  # Median of |m| over the root's last 250 sessions (at least 100 with a value).
  rel mmed(+A: Equity, @T: Timestamp, -MM: Scalar)
  mmed(A, T, MM) :- session(A, T),
      MM = median(X) over (T1 in rows(T, 250, min 100), session(A, T1), am_d(A, T1, X)).
}

strategy r8l {
  env futures_sessions
  uses r8l_daily
  resolution @1m
  mode delta

  param risk : Notional<USD> = 6493.912071090746 USD
  param k_main : Scalar = 2.0
  param gap_side : Scalar = -0.75
  param late_den : Scalar = 3.0

  # Everything a decision at the decision time reads: the previous session's
  # daily features (exact previous session, never carried forward), the
  # multiplier, and today's prints (labelled after the session open).
  rel ctx(+A: Equity, @T: Timestamp, -ATR: Price<USD>, -PC: Price<USD>, -MM: Scalar,
          -MU: Scalar, -O0: Price<USD>, -C30: Price<USD>, -C20: Price<USD>)
  ctx(A, T, ATR, PC, MM, MU, O0, C30, C20) :- clock(A, T, S),
      session(A, P) asof T,
      atr(A, K1, ATR) asof T, K1 = P, ATR > 0 USD/share,
      close_d(A, K2, PC) asof T, K2 = P,
      mmed(A, K3, MM) asof T, K3 = P,
      multiplier(A, K4, MU) asof T, K4 = P,
      open0_m(A, T1, O0) asof T, S < T1,
      close30_m(A, T2, C30) asof T, S < T2,
      close20_m(A, T3, C20) asof T, S < T3.

  # Main, long: m > 0 and |m| >= 2 m_med; gap aligned by |g| < 1;
  # latefrac <= 1/3 (late_den = 3, so latefrac <= 1 / late_den).
  decide(T, buy(A, Q, moo_moc)) :- clock(A, T, _),
      ctx(A, T, ATR, PC, MM, MU, O0, C30, C20),
      M = (C30 - O0) / ATR, M > 0,
      G = (O0 - PC) / ATR, GA = abs(G), GA < 1,
      M >= k_main * MM,
      L = (C30 - C20) / (C30 - O0), L <= 1 / late_den,
      Q = risk / (ATR * MU).

  # Main, long, gap aligned by the same sign: g >= 1.
  decide(T, buy(A, Q, moo_moc)) :- clock(A, T, _),
      ctx(A, T, ATR, PC, MM, MU, O0, C30, C20),
      M = (C30 - O0) / ATR, M > 0,
      G = (O0 - PC) / ATR, GA = abs(G), GA >= 1, G > 0,
      M >= k_main * MM,
      L = (C30 - C20) / (C30 - O0), L <= 1 / late_den,
      Q = risk / (ATR * MU).

  # Main, short: m < 0 and |m| >= 2 m_med; gap aligned by |g| < 1.
  decide(T, short(A, Q, moo_moc)) :- clock(A, T, _),
      ctx(A, T, ATR, PC, MM, MU, O0, C30, C20),
      M = (C30 - O0) / ATR, M < 0,
      G = (O0 - PC) / ATR, GA = abs(G), GA < 1,
      AM = abs(M), AM >= k_main * MM,
      L = (C30 - C20) / (C30 - O0), L <= 1 / late_den,
      Q = risk / (ATR * MU).

  # Main, short, gap aligned by the same sign: g <= -1.
  decide(T, short(A, Q, moo_moc)) :- clock(A, T, _),
      ctx(A, T, ATR, PC, MM, MU, O0, C30, C20),
      M = (C30 - O0) / ATR, M < 0,
      G = (O0 - PC) / ATR, GA = abs(G), GA >= 1, G < 0,
      AM = abs(M), AM >= k_main * MM,
      L = (C30 - C20) / (C30 - O0), L <= 1 / late_den,
      Q = risk / (ATR * MU).

  # Sidecar: |m| < 2 m_med and g <= -0.75, short.
  decide(T, short(A, Q, moo_moc)) :- clock(A, T, _),
      ctx(A, T, ATR, PC, MM, MU, O0, C30, C20),
      M = (C30 - O0) / ATR, AM = abs(M), AM < k_main * MM,
      G = (O0 - PC) / ATR, G <= gap_side,
      Q = risk / (ATR * MU).
}
