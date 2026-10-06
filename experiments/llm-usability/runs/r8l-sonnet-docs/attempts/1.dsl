library r8l_daily {
  env futures_sessions
  resolution @1d

  rel tr(+A: Equity, @T: Timestamp, -R: Price<USD>)
  tr(A, T, R) :- session(A, T), high_d(A, T, H), low_d(A, T, L), close_d(A, T, C),
      prev(T, Tp), close_d(A, _, PC) asof Tp,
      R = greatest(greatest(H - L, abs(H - PC)), abs(L - PC)).

  rel atr(+A: Equity, @T: Timestamp, -V: Price<USD>)
  atr(A, T, V) :- session(A, T),
      V = mean(R) over (T1 in rows(T, 20, min 20), session(A, T1), tr(A, T1, R)).

  rel mo(-A: Equity, @T: Timestamp, -O: Price<USD>)
  mo(A, T, O) :- resample(open0_m(A, T1, P) to @1d as T, min 1, O = last(P)).

  rel mc(-A: Equity, @T: Timestamp, -C: Price<USD>)
  mc(A, T, C) :- resample(close30_m(A, T1, P) to @1d as T, min 1, C = last(P)).

  rel mval(+A: Equity, @T: Timestamp, -M: Scalar)
  mval(A, T, M) :- session(A, T), mo(A, T, O), mc(A, T, C), prev(T, Tp),
      session(A, Tq) asof Tp, atr(A, Tq2, Atr) asof Tp, Tq2 = Tq, Atr > 0 USD/share,
      M = (C - O) / Atr.

  rel absm(+A: Equity, @T: Timestamp, -V: Scalar)
  absm(A, T, V) :- mval(A, T, M), V = abs(M).

  rel med(+A: Equity, @T: Timestamp, -V: Scalar)
  med(A, T, V) :- session(A, T),
      V = median(X) over (T1 in rows(T, 250, min 100), session(A, T1), absm(A, T1, X)).

  rel feat(+A: Equity, @T: Timestamp, -Atr: Price<USD>, -Med: Scalar, -PC: Price<USD>)
  feat(A, T, Atr, Med, PC) :- session(A, T), atr(A, T, Atr), med(A, T, Med), close_d(A, T, PC).
}

strategy r8l {
  env futures_sessions
  uses r8l_daily
  resolution @1m
  mode delta
  param risk : Notional<USD> = 6493.912071090746 USD
  param k : Scalar = 2
  param gapmin : Scalar = 0.75

  rel ctx(-A: Equity, @T: Timestamp, -Atr: Price<USD>, -Med: Scalar, -Mu: Scalar,
          -O0: Price<USD>, -C20: Price<USD>, -C30: Price<USD>, -PC: Price<USD>)
  ctx(A, T, Atr, Med, Mu, O0, C20, C30, PC) :- clock(A, T, S),
      feat(A, Tf, Atr, Med, PC) asof T,
      session(A, Tp) asof T, Tf = Tp,
      multiplier(A, _, Mu) asof T,
      open0_m(A, T0, O0) asof T, S < T0,
      close20_m(A, T2, C20) asof T, S < T2,
      close30_m(A, T3, C30) asof T, S < T3,
      Atr > 0 USD/share.

  decide(T, buy(A, Q, moo_moc)) :- ctx(A, T, Atr, Med, Mu, O0, C20, C30, PC),
      M = (C30 - O0) / Atr, G = (O0 - PC) / Atr,
      M > 0, abs(M) >= k * Med, G > -1,
      3 * (C30 - C20) <= C30 - O0,
      Q = risk / (Atr * Mu).

  decide(T, short(A, Q, moo_moc)) :- ctx(A, T, Atr, Med, Mu, O0, C20, C30, PC),
      M = (C30 - O0) / Atr, G = (O0 - PC) / Atr,
      M < 0, abs(M) >= k * Med, G < 1,
      3 * (C30 - C20) >= C30 - O0,
      Q = risk / (Atr * Mu).

  decide(T, short(A, Q, moo_moc)) :- ctx(A, T, Atr, Med, Mu, O0, C20, C30, PC),
      M = (C30 - O0) / Atr, G = (O0 - PC) / Atr,
      abs(M) < k * Med, G <= 0 - gapmin,
      Q = risk / (Atr * Mu).
}
