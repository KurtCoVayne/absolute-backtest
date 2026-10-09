# R8L: intraday futures opening-range book (TASK.md rules).
strategy r8l {
  env futures_sessions
  resolution @1m
  mode delta

  # Constants of the book, lifted into parameters.
  param mmult : Scalar = 2
  param gap_band : Scalar = 1
  param sc_gap : Scalar = -0.75
  param lf_max : Scalar = 0.3333333333333333
  param atr_n : Count = 20
  param mwin : Count = 250
  param mmin : Count = 100
  param risk : Notional<USD> = 6493.912071090746 USD

  # Daily true range of each root session (the root's first session has none).
  rel trd(+A: Equity, @T: Timestamp, -R: Price<USD>) @1d
  trd(A, T, R) :- session(A, T), prev(T, P), session(A, T0) asof P,
      close_d(A, T0, C0), high_d(A, T, H), low_d(A, T, L),
      R = greatest(H - L, greatest(abs(H - C0), abs(L - C0))).

  # ATR: mean true range over the root's last atr_n sessions, all of them present.
  rel atrd(+A: Equity, @T: Timestamp, -V: Price<USD>) @1d
  atrd(A, T, V) :- session(A, T),
      V = mean(R) over (T1 in rows(T, atr_n, min atr_n), session(A, T1), trd(A, T1, R)).

  # The close before minute 30 and the open of minute 0, one row per session.
  rel c30d(+A: Equity, @T: Timestamp, -C: Price<USD>) @1d
  c30d(A, T, C) :- resample(close30_m(A, T1, P) to @1d as T, min 1, C = last(P)).
  rel o0d(+A: Equity, @T: Timestamp, -O: Price<USD>) @1d
  o0d(A, T, O) :- resample(open0_m(A, T1, P) to @1d as T, min 1, O = first(P)).

  # m of the session, scaled by the previous session's ATR; absent when that ATR is not positive.
  rel md(+A: Equity, @T: Timestamp, -M: Scalar) @1d
  md(A, T, M) :- c30d(A, T, C), o0d(A, T, O), prev(T, P), session(A, T0) asof P,
      atrd(A, T0, ATR), ATR > 0 USD/share, M = (C - O) / ATR.

  # m_med: median of |m| over the root's last mwin sessions, at least mmin with a value.
  rel mmedd(+A: Equity, @T: Timestamp, -V: Scalar) @1d
  mmedd(A, T, V) :- session(A, T),
      V = median(abs(M)) over (T1 in rows(T, mwin, min mmin), session(A, T1), md(A, T1, M)).

  # The session's features as one row: present only when all of them are.
  rel sfeat(+A: Equity, @T: Timestamp, -ATR: Price<USD>, -MM: Scalar, -C: Price<USD>, -MU: Scalar) @1d
  sfeat(A, T, ATR, MM, C, MU) :- session(A, T), atrd(A, T, ATR), mmedd(A, T, MM),
      close_d(A, T, C), multiplier(A, T, MU).

  # Today's prints at the decision time T. Each must fall inside the session that opened at S,
  # so a missing print of today cannot be replaced by an earlier session's print.
  rel dayv(-A: Equity, @T: Timestamp, -O: Price<USD>, -C20: Price<USD>, -C30: Price<USD>)
  dayv(A, T, O, C20, C30) :- clock(A, T, S),
      open0_m(A, T1, O) asof T, S < T1,
      close20_m(A, T2, C20) asof T, S < T2,
      close30_m(A, T3, C30) asof T, S < T3.

  # latefrac, defined only where its denominator is nonzero.
  rel lfr(-A: Equity, @T: Timestamp, -X: Scalar)
  lfr(A, T, X) :- dayv(A, T, O, C20, C30), LD = C30 - O, LD > 0 USD/share,
      X = (C30 - C20) / LD.
  lfr(A, T, X) :- dayv(A, T, O, C20, C30), LD = C30 - O, LD < 0 USD/share,
      X = (C30 - C20) / LD.

  # The previous session's features as of T: the latest session row must be the previous session.
  rel prevf(-A: Equity, @T: Timestamp, -ATR: Price<USD>, -MM: Scalar, -C0: Price<USD>, -MU: Scalar)
  prevf(A, T, ATR, MM, C0, MU) :- clock(A, T, _), session(A, S0) asof T,
      sfeat(A, S1, ATR, MM, C0, MU) asof T, S1 = S0.

  # Main long: m >= 2 m_med, gap aligned (|g| < 1 or g > 0, i.e. g > -1), latefrac <= 1/3.
  decide(T, buy(A, Q, moo_moc)) :- dayv(A, T, O, _, C30), prevf(A, T, ATR, MM, C0, MU),
      lfr(A, T, LF), ATR > 0 USD/share, M = (C30 - O) / ATR, M > 0,
      AM = abs(M), AM >= mmult * MM,
      GB = (O - C0) / ATR + gap_band, GB > 0, LF <= lf_max,
      Q = risk / (ATR * MU).

  # Main short: m <= -2 m_med, gap aligned (|g| < 1 or g < 0, i.e. g < 1), latefrac <= 1/3.
  decide(T, short(A, Q, moo_moc)) :- dayv(A, T, O, _, C30), prevf(A, T, ATR, MM, C0, MU),
      lfr(A, T, LF), ATR > 0 USD/share, M = (C30 - O) / ATR, M < 0,
      AM = abs(M), AM >= mmult * MM,
      GS = gap_band - (O - C0) / ATR, GS > 0, LF <= lf_max,
      Q = risk / (ATR * MU).

  # Sidecar: |m| < 2 m_med and g <= -0.75, short.
  decide(T, short(A, Q, moo_moc)) :- dayv(A, T, O, _, C30), prevf(A, T, ATR, MM, C0, MU),
      ATR > 0 USD/share, M = (C30 - O) / ATR,
      AM = abs(M), AM < mmult * MM,
      G = (O - C0) / ATR, G <= sc_gap,
      Q = risk / (ATR * MU).
}
