strategy r8l {
  env futures_sessions
  resolution @1m
  mode delta

  # Daily true range: max(high - low, |high - prev_close|, |low - prev_close|)
  rel tr(-A: Equity, @T: Timestamp, -TR: Price<USD>)
  tr(A, T, TR) :- session(A, T), high_d(A, T, H), low_d(A, T, L),
      TR = H - L.
  tr(A, T, TR) :- session(A, T), high_d(A, T, H), low_d(A, T, L),
      prev(T, T0), close_d(A, T0, P0),
      TR1 = abs(H - P0), TR2 = abs(L - P0),
      TR = greatest(H - L, TR1, TR2).

  # ATR: mean true range over last 20 sessions
  rel atr(-A: Equity, @T: Timestamp, -ATR: Price<USD>)
  atr(A, T, ATR) :- session(A, T),
      ATR = mean(TR) over (T1 in window(T, 0d, min 20), tr(A, T1, TR)).

  # At decision time, read previous session's ATR using asof
  rel prev_atr(-A: Equity, @T: Timestamp, -ATR: Price<USD>)
  prev_atr(A, T, ATR) :- clock(A, T, _), atr(A, TP, ATR) asof T.

  # m = (close_before_30 - open_0) / ATR
  rel m_val(-A: Equity, @T: Timestamp, -M: Scalar)
  m_val(A, T, M) :- clock(A, T, _), close30_m(A, T, C30), open0_m(A, T, O0),
      prev_atr(A, T, ATR), ATR > 0 USD,
      M = (C30 - O0) / ATR.

  # g = (open_0 - prev_close) / ATR
  rel g_val(-A: Equity, @T: Timestamp, -G: Scalar)
  g_val(A, T, G) :- clock(A, T, S), open0_m(A, T, O0),
      prev(S, SP), close_d(A, SP, P0),
      prev_atr(A, T, ATR), ATR > 0 USD,
      G = (O0 - P0) / ATR.

  # m_med: median of |m| over last 250 sessions (at least 100 with values)
  rel m_med(-A: Equity, @T: Timestamp, -MMED: Scalar)
  m_med(A, T, MMED) :- clock(A, T, _),
      MMED = median(M) over (T1 in window(T, 0d, min 100),
                              M = abs(MV),
                              m_val(A, T1, MV)).

  # latefrac = (close_before_30 - close_before_20) / (close_before_30 - open_0)
  rel latefrac(-A: Equity, @T: Timestamp, -LF: Scalar)
  latefrac(A, T, LF) :- clock(A, T, _), close30_m(A, T, C30), close20_m(A, T, C20),
      open0_m(A, T, O0),
      LF = (C30 - C20) / (C30 - O0).

  # Main entry: |m| >= 2 * m_med AND gap aligned AND latefrac <= 1/3
  rel main_entry(-A: Equity, @T: Timestamp, -DIR: Scalar)
  main_entry(A, T, DIR) :- clock(A, T, _), m_val(A, T, M), m_med(A, T, MMED),
      g_val(A, T, G), latefrac(A, T, LF),
      abs(M) >= 2.0 * MMED,
      (abs(G) < 1.0 or (G > 0 and M > 0) or (G < 0 and M < 0)),
      LF <= 0.333333,
      DIR = 1.0 if M > 0 else -1.0.

  # Sidecar entry: |m| < 2 * m_med AND g <= -0.75
  rel sidecar_entry(-A: Equity, @T: Timestamp, -DIR: Scalar)
  sidecar_entry(A, T, -1.0) :- clock(A, T, _), m_val(A, T, M), m_med(A, T, MMED),
      g_val(A, T, G),
      abs(M) < 2.0 * MMED,
      G <= -0.75.

  # Sizing: contracts = direction * D / (ATR * multiplier)
  # D = 6493.912071090746
  rel entry_qty(-A: Equity, @T: Timestamp, -Q: Quantity<Shares>)
  entry_qty(A, T, Q) :- main_entry(A, T, DIR),
      prev_atr(A, T, ATR), multiplier(A, T, M), ATR > 0 USD,
      D = 6493.912071090746 USD,
      Q = DIR * D / (ATR * M).
  entry_qty(A, T, Q) :- sidecar_entry(A, T, DIR),
      prev_atr(A, T, ATR), multiplier(A, T, M), ATR > 0 USD,
      D = 6493.912071090746 USD,
      Q = DIR * D / (ATR * M).

  # Decisions: enter at open_m, exit at close_m
  decide(T, buy(A, Q)) :- clock(A, T, _), main_entry(A, T, DIR), DIR > 0,
      entry_qty(A, T, QQ), Q = abs(QQ).
  decide(T, short(A, Q)) :- clock(A, T, _), main_entry(A, T, DIR), DIR < 0,
      entry_qty(A, T, QQ), Q = abs(QQ).
  decide(T, short(A, Q)) :- clock(A, T, _), sidecar_entry(A, T, _),
      entry_qty(A, T, Q).

  # Exit: sell all at session close
  decide(T, sell(A, Q)) :- close_m(A, T, _), position(A, T, Q), Q > 0 shares.
  decide(T, cover(A, Q)) :- close_m(A, T, _), position(A, T, Q), Q < 0 shares.
}
