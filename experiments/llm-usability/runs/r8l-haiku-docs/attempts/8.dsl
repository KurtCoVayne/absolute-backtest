strategy r8l {
  env futures_sessions
  resolution @1m
  mode delta

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
    ATR = mean(TR) over (T1 in window(T, 20d, min 20), true_range(A, T1, TR)).

  # At @1m, read previous session's ATR via as-of join
  rel atr_prev(+A: Equity, @T: Timestamp, -ATR: Price<USD>)
  atr_prev(A, T, ATR) :-
    open_m(A, T, _),
    atr(A, T0, ATR) asof T,
    T0 < T.

  # Read multiplier for current session via as-of join
  rel multiplier_at_time(+A: Equity, @T: Timestamp, -M: Scalar)
  multiplier_at_time(A, T, M) :-
    open_m(A, T, _),
    multiplier(A, T0, M) asof T.

  # Read previous session's close via as-of join
  rel prev_close(+A: Equity, @T: Timestamp, -C: Price<USD>)
  prev_close(A, T, C) :-
    open_m(A, T, _),
    close_d(A, T0, C) asof T,
    T0 < T.

  # m = (close before minute 30 - open of minute 0) / ATR
  rel m_value(+A: Equity, @T: Timestamp, -M: Scalar)
  m_value(A, T, M) :-
    clock(A, T, S),
    close30_m(A, T, C30),
    open0_m(A, T, O0),
    atr_prev(A, T, ATR),
    M = (C30 - O0) / ATR.

  # g (gap) = (open of minute 0 - previous session's close) / ATR
  rel gap_value(+A: Equity, @T: Timestamp, -G: Scalar)
  gap_value(A, T, G) :-
    clock(A, T, S),
    open0_m(A, T, O0),
    prev_close(A, T, C0),
    atr_prev(A, T, ATR),
    G = (O0 - C0) / ATR.

  # m_med: median of |m| over last 250 sessions (at least 100 sessions with value)
  rel m_med(+A: Equity, @T: Timestamp, -MED: Scalar)
  m_med(A, T, MED) :-
    clock(A, T, S),
    N = count(M) over (T1 in window(T, 250d, min 1), m_value(A, T1, M)),
    N >= 100,
    MED = median(abs(M)) over (T1 in window(T, 250d, min 1), m_value(A, T1, M)).

  # latefrac = (close before minute 30 - close before minute 20) / (close before minute 30 - open of minute 0)
  rel latefrac(+A: Equity, @T: Timestamp, -LF: Scalar)
  latefrac(A, T, LF) :-
    clock(A, T, S),
    close30_m(A, T, C30),
    close20_m(A, T, C20),
    open0_m(A, T, O0),
    Denom = C30 - O0,
    LF = (C30 - C20) / Denom.

  # Gap is small in absolute terms
  rel gap_small(+A: Equity, @T: Timestamp)
  gap_small(A, T) :-
    gap_value(A, T, G),
    abs(G) < 1.

  # Gap and m have the same sign
  rel gap_m_aligned(+A: Equity, @T: Timestamp)
  gap_m_aligned(A, T) :-
    gap_value(A, T, G),
    m_value(A, T, M),
    G > 0,
    M > 0.

  gap_m_aligned(A, T) :-
    gap_value(A, T, G),
    m_value(A, T, M),
    G < 0,
    M < 0.

  # Gap is aligned: small OR same sign as m
  rel gap_aligned(+A: Equity, @T: Timestamp)
  gap_aligned(A, T) :- gap_small(A, T).
  gap_aligned(A, T) :- gap_m_aligned(A, T).

  # Main entry: |m| >= 2 x m_med AND gap aligned AND latefrac <= 1/3
  rel main_entry(+A: Equity, @T: Timestamp, -Dir: Scalar)
  main_entry(A, T, Dir) :-
    clock(A, T, S),
    m_value(A, T, M),
    m_med(A, T, MED),
    abs(M) >= 2 * MED,
    gap_aligned(A, T),
    latefrac(A, T, LF),
    LF <= 1 / 3,
    M > 0,
    Dir = 1.

  main_entry(A, T, Dir) :-
    clock(A, T, S),
    m_value(A, T, M),
    m_med(A, T, MED),
    abs(M) >= 2 * MED,
    gap_aligned(A, T),
    latefrac(A, T, LF),
    LF <= 1 / 3,
    M < 0,
    Dir = -1.

  # Sidecar entry: |m| < 2 x m_med AND g <= -0.75
  rel sidecar_entry(+A: Equity, @T: Timestamp)
  sidecar_entry(A, T) :-
    clock(A, T, S),
    m_value(A, T, M),
    m_med(A, T, MED),
    abs(M) < 2 * MED,
    gap_value(A, T, G),
    G <= -0.75.

  # Sizing: contracts = D / (ATR * multiplier)
  # where D = 6493.912071090746 USD
  rel size_at_decision(+A: Equity, @T: Timestamp, -Q: Quantity<Shares>)
  size_at_decision(A, T, Q) :-
    clock(A, T, S),
    atr_prev(A, T, ATR),
    multiplier_at_time(A, T, M),
    D = 6493.912071090746 USD,
    Q = D / (ATR * M).

  # Entry decisions
  decide(T, buy(A, Q)) :-
    clock(A, T, S),
    main_entry(A, T, 1),
    size_at_decision(A, T, Q).

  decide(T, short(A, Q)) :-
    clock(A, T, S),
    main_entry(A, T, -1),
    size_at_decision(A, T, Q).

  decide(T, short(A, Q)) :-
    clock(A, T, S),
    sidecar_entry(A, T),
    size_at_decision(A, T, Q).

  # Exit at session close: position exits at last close_m of the session
  decide(T, sell(A, Q)) :-
    position(A, T, Q),
    Q > 0,
    close_m(A, T, _),
    prev(T, T0),
    clock(A, T0, S0),
    S0 < T.

  decide(T, cover(A, Q)) :-
    position(A, T, Q),
    Q < 0,
    close_m(A, T, _),
    prev(T, T0),
    clock(A, T0, S0),
    S0 < T.
}
