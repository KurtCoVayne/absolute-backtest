strategy r8l {
  env futures_sessions
  resolution @1m
  mode delta

  # Daily features (at @1d)
  rel tr(-A: Equity, @T: Timestamp, -TR: Price<USD>) @1d
  tr(A, T, TR) :- session(A, T), high_d(A, T, H), low_d(A, T, L),
      prev(T, T0), close_d(A, T0, P0),
      TR1 = abs(H - P0), TR2 = abs(L - P0),
      TR = greatest(H - L, TR1, TR2).
  tr(A, T, TR) :- session(A, T), high_d(A, T, H), low_d(A, T, L),
      not has_prev(T),
      TR = H - L.

  rel has_prev(@T: Timestamp)
  has_prev(T) :- session(_, T), prev(T, _).

  rel atr(-A: Equity, @T: Timestamp, -ATR: Price<USD>) @1d
  atr(A, T, ATR) :- session(A, T),
      ATR = mean(TR) over (T1 in window(T, 0d, min 20), tr(A, T1, TR)).

  # At decision time, read previous session's ATR using asof
  rel prev_atr(-A: Equity, @T: Timestamp, -ATR: Price<USD>)
  prev_atr(A, T, ATR) :- clock(A, T, _), atr(A, TP, ATR) asof T.

  # Read previous close
  rel prev_close(-A: Equity, @T: Timestamp, -P: Price<USD>)
  prev_close(A, T, P) :- clock(A, T, S), prev(S, SP), close_d(A, SP, P).

  # Read multiplier
  rel mult(-A: Equity, @T: Timestamp, -M: Scalar)
  mult(A, T, M) :- clock(A, T, _), multiplier(A, T, M).

  # m = (close_before_30 - open_0) / ATR
  rel m_val(-A: Equity, @T: Timestamp, -M: Scalar)
  m_val(A, T, M) :- clock(A, T, _), close30_m(A, T, C30), open0_m(A, T, O0),
      prev_atr(A, T, ATR), ATR > 0 USD,
      M = (C30 - O0) / ATR.

  # g = (open_0 - prev_close) / ATR
  rel g_val(-A: Equity, @T: Timestamp, -G: Scalar)
  g_val(A, T, G) :- clock(A, T, _), open0_m(A, T, O0),
      prev_close(A, T, P0),
      prev_atr(A, T, ATR), ATR > 0 USD,
      G = (O0 - P0) / ATR.

  # m_med: median of |m| over last window (at least 100 with values)
  rel m_abs(-A: Equity, @T: Timestamp, -MABS: Scalar)
  m_abs(A, T, MABS) :- m_val(A, T, M), MABS = abs(M).

  rel m_med(-A: Equity, @T: Timestamp, -MMED: Scalar)
  m_med(A, T, MMED) :- clock(A, T, _),
      MMED = median(MABS) over (T1 in window(T, 0d, min 100), m_abs(A, T1, MABS)).

  # latefrac = (close_before_30 - close_before_20) / (close_before_30 - open_0)
  rel latefrac(-A: Equity, @T: Timestamp, -LF: Scalar)
  latefrac(A, T, LF) :- clock(A, T, _), close30_m(A, T, C30), close20_m(A, T, C20),
      open0_m(A, T, O0),
      LF = (C30 - C20) / (C30 - O0).

  # Check if gap aligned by absolute value
  rel gap_aligned_abs(-A: Equity, @T: Timestamp)
  gap_aligned_abs(A, T) :- g_val(A, T, G), G_ABS = abs(G), G_ABS < 1.0.

  # Check if gap aligned by same sign
  rel gap_aligned_pos(-A: Equity, @T: Timestamp)
  gap_aligned_pos(A, T) :- m_val(A, T, M), g_val(A, T, G), M > 0, G > 0.

  rel gap_aligned_neg(-A: Equity, @T: Timestamp)
  gap_aligned_neg(A, T) :- m_val(A, T, M), g_val(A, T, G), M < 0, G < 0.

  rel gap_aligned(-A: Equity, @T: Timestamp)
  gap_aligned(A, T) :- gap_aligned_abs(A, T).
  gap_aligned(A, T) :- gap_aligned_pos(A, T).
  gap_aligned(A, T) :- gap_aligned_neg(A, T).

  # Main entry (long): |m| >= 2 * m_med AND gap aligned AND latefrac <= 1/3 AND m > 0
  rel main_long(-A: Equity, @T: Timestamp)
  main_long(A, T) :- clock(A, T, _), m_val(A, T, M), m_med(A, T, MMED),
      gap_aligned(A, T), latefrac(A, T, LF),
      M_ABS = abs(M), M_ABS >= 2.0 * MMED,
      LF <= 0.333333,
      M > 0.

  # Main entry (short): |m| >= 2 * m_med AND gap aligned AND latefrac <= 1/3 AND m < 0
  rel main_short(-A: Equity, @T: Timestamp)
  main_short(A, T) :- clock(A, T, _), m_val(A, T, M), m_med(A, T, MMED),
      gap_aligned(A, T), latefrac(A, T, LF),
      M_ABS = abs(M), M_ABS >= 2.0 * MMED,
      LF <= 0.333333,
      M < 0.

  # Sidecar entry (always short): |m| < 2 * m_med AND g <= -0.75
  rel sidecar(-A: Equity, @T: Timestamp)
  sidecar(A, T) :- clock(A, T, _), m_val(A, T, M), m_med(A, T, MMED),
      g_val(A, T, G),
      M_ABS = abs(M), M_ABS < 2.0 * MMED,
      G <= -0.75.

  # Sizing: contracts = direction * D / (ATR * multiplier)
  # D = 6493.912071090746
  rel entry_qty_long(-A: Equity, @T: Timestamp, -Q: Quantity<Shares>)
  entry_qty_long(A, T, Q) :- main_long(A, T),
      prev_atr(A, T, ATR), mult(A, T, M), ATR > 0 USD,
      D = 6493.912071090746 USD,
      Q = D / (ATR * M).

  rel entry_qty_short(-A: Equity, @T: Timestamp, -Q: Quantity<Shares>)
  entry_qty_short(A, T, Q) :- main_short(A, T),
      prev_atr(A, T, ATR), mult(A, T, M), ATR > 0 USD,
      D = 6493.912071090746 USD,
      Q = D / (ATR * M).
  entry_qty_short(A, T, Q) :- sidecar(A, T),
      prev_atr(A, T, ATR), mult(A, T, M), ATR > 0 USD,
      D = 6493.912071090746 USD,
      Q = D / (ATR * M).

  # Decisions: enter at clock time, exit at close_m
  decide(T, buy(A, Q)) :- main_long(A, T), entry_qty_long(A, T, Q).
  decide(T, short(A, Q)) :- main_short(A, T), entry_qty_short(A, T, Q).
  decide(T, short(A, Q)) :- sidecar(A, T), entry_qty_short(A, T, Q).

  # Exit: sell all at session close
  rel has_position(-A: Equity, @T: Timestamp)
  has_position(A, T) :- position(A, T, Q), Q > 0 shares.
  has_position(A, T) :- position(A, T, Q), Q < 0 shares.

  rel exit_time(-A: Equity, @T: Timestamp)
  exit_time(A, T) :- close_m(A, T, _).

  decide(T, sell(A, Q)) :- exit_time(A, T), position(A, T, Q), Q > 0 shares.
  decide(T, cover(A, Q)) :- exit_time(A, T), position(A, T, Q), Q < 0 shares.
}
