library r8l_features {
  env futures_sessions
  resolution @1d

  # True range at @1d: max(high - low, |high - prev close|, |low - prev close|)
  rel true_range(+A: Equity, @T: Timestamp, -TR: Price<USD>)
  true_range(A, T, TR) :-
    high_d(A, T, H),
    low_d(A, T, L),
    close_d(A, T, C),
    prev(T, T0),
    close_d(A, T0, C0),
    HL = H - L,
    HC = abs(H - C0),
    LC = abs(L - C0),
    TR = greatest(HL, HC, LC).

  # ATR: mean of true range over last 20 sessions
  rel atr(+A: Equity, @T: Timestamp, -ATR: Price<USD>)
  atr(A, T, ATR) :-
    close_d(A, T, _),
    ATR = mean(TR) over (T1 in window(T, 20d, min 20), true_range(A, T1, TR)).
}
