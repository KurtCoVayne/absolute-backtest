strategy r8l {
  env futures_sessions
  resolution @1m
  mode delta

  param atr_n : Count = 20
  param med_n : Count = 250
  param med_min : Count = 100
  param med_mult : Scalar = 2.0
  param gap_cap : Scalar = 1.0
  param late_max : Scalar = 0.3333333333333333
  param side_gap : Scalar = 0.75
  param risk : Notional<USD> = 6493.912071090746 USD

  # ---- daily features (@1d), keyed by the session they describe ----
  rel tr(+A: Equity, @T: Timestamp, -X: Price<USD>) @1d
  tr(A, T, X) :- session(A, T), high_d(A, T, H), low_d(A, T, L),
      prev(T, T1), close_d(A, _, PC) asof T1,
      X = greatest(H - L, abs(H - PC), abs(L - PC)).

  rel atr(+A: Equity, @T: Timestamp, -X: Price<USD>) @1d
  atr(A, T, X) :- session(A, T),
      X = mean(Y) over (T1 in rows(T, atr_n, min atr_n), session(A, T1), tr(A, T1, Y)).

  rel o0d(+A: Equity, @T: Timestamp, -P: Price<USD>) @1d
  o0d(A, T, P) :- resample(open0_m(A, T1, Q) to @1d as T, min 1, P = first(Q)).
  rel c30d(+A: Equity, @T: Timestamp, -P: Price<USD>) @1d
  c30d(A, T, P) :- resample(close30_m(A, T1, Q) to @1d as T, min 1, P = last(Q)).

  # |m| of a session, against the ATR of the session before it
  rel absm(+A: Equity, @T: Timestamp, -V: Scalar) @1d
  absm(A, T, V) :- o0d(A, T, O), c30d(A, T, C), prev(T, T1), session(A, TP) asof T1,
      atr(A, TP, ATR), ATR > 0 USD/share, V = abs((C - O) / ATR).

  rel mmed(+A: Equity, @T: Timestamp, -V: Scalar) @1d
  mmed(A, T, V) :- session(A, T),
      V = median(X) over (T1 in rows(T, med_n, min med_min), session(A, T1), absm(A, T1, X)).

  # ---- decision-time signal ----
  rel sig(-A: Equity, @T: Timestamp, -M: Scalar, -G: Scalar, -L: Scalar, -MM: Scalar, -ATR: Price<USD>)
  sig(A, T, M, G, L, MM, ATR) :- clock(A, T, S),
      session(A, TS) asof T,
      atr(A, T0, ATR) asof T, TS = T0, ATR > 0 USD/share,
      mmed(A, T1, MM) asof T, TS = T1,
      close_d(A, T2, PC) asof T, TS = T2,
      open0_m(A, K0, O0) asof T, S < K0,
      close20_m(A, K2, C20) asof T, S < K2,
      close30_m(A, K3, C30) asof T, S < K3,
      D = C30 - O0, abs(D) > 0 USD/share,
      M = D / ATR, G = (O0 - PC) / ATR, L = (C30 - C20) / D.

  rel aligned(-A: Equity, @T: Timestamp)
  aligned(A, T) :- sig(A, T, M, G, _, _, _), abs(G) < gap_cap.
  aligned(A, T) :- sig(A, T, M, G, _, _, _), G > 0, M > 0.
  aligned(A, T) :- sig(A, T, M, G, _, _, _), G < 0, M < 0.

  rel size(-A: Equity, @T: Timestamp, +ATR: Price<USD>, -Q: Quantity<Shares>)
  size(A, T, ATR, Q) :- clock(A, T, _), multiplier(A, T0, MU) asof T, Q = risk / (ATR * MU).

  decide(T, buy(A, Q, moo_moc)) :- sig(A, T, M, _, L, MM, ATR), abs(M) >= med_mult * MM,
      M > 0, aligned(A, T), L <= late_max, size(A, T, ATR, Q).
  decide(T, short(A, Q, moo_moc)) :- sig(A, T, M, _, L, MM, ATR), abs(M) >= med_mult * MM,
      M < 0, aligned(A, T), L <= late_max, size(A, T, ATR, Q).
  decide(T, short(A, Q, moo_moc)) :- sig(A, T, M, G, _, MM, ATR), abs(M) < med_mult * MM,
      G <= 0 - side_gap, size(A, T, ATR, Q).
}
