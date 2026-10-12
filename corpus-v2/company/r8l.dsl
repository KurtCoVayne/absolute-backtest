# MORNIGHT-R8L: the intraday futures opening-range book (d20-research PB-5
# amendment 2026-09-02, ledger row 150; scripts/r31_return.py `load`,
# scripts/r37_book.py `build_basic` with latefrac_max 1/3, no_stop, no_late;
# scripts/r39_fixed.py). One leg per root and session at most.
# `docs/language-v2.md` section 9.2 reads this file against the book's rules.
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
#            dollars / (ATR x point value) contracts, fractional
#
# Known difference from the Python book: where minute 30 did not trade, the
# entry is the next traded minute's open, where the book fills at the close
# before minute 30 (231 of 21,892 legs); a session with no print after minute
# 29 makes no leg here, a commission-only leg there.
# allow: W9  (the instruments table is the book's HOME-26 list with its start years: a hindsight constant, counted)
strategy r8l {
  data {
    class    future
    need     bars @1m from 2000-01-01 complete
    need     sessions, point_value, commission
    require  tested
    prefer   latest
  }
  uses      calendar, bars
  calendars {
    session = sessions                 # each root's own primary sessions
    es_days = sessions of "ES"         # the reporting calendar
  }
  clock { decide on base at session + 30min }   # the bar labelled open + 30 minutes, or the first traded bar after it

  universe {
    # The HOME-26 roots, each from the first year the book trades it.
    instruments {
      ES from 2000-01-01;  NQ from 2000-01-01;  KC from 2001-01-01;  6J from 2001-01-01
      ZB from 2001-01-01;  ZF from 2001-01-01;  ZN from 2001-01-01;  RTY from 2002-01-01
      ZT from 2003-01-01;  CL from 2007-01-01;  GC from 2007-01-01;  HE from 2007-01-01
      HO from 2007-01-01;  LE from 2007-01-01;  NG from 2007-01-01;  PL from 2007-01-01
      RB from 2007-01-01;  SI from 2007-01-01;  ZC from 2007-01-01;  ZL from 2007-01-01
      ZM from 2007-01-01;  ZS from 2007-01-01;  ZW from 2007-01-01;  GF from 2008-01-01
      SB from 2008-01-01;  ETH
    }
    universe(A, T) :- instruments(A, From), session_of(A, T, _), T >= From.
  }

  params {
    atr_rows  : Count = 20
    med_rows  : Count = 250
    med_min   : Count = 100
    k_main    : Scalar = 2
    gap_free  : Scalar = 1
    late_max  : Scalar = 1/3
    side_gap  : Scalar = -0.75
    risk      : Notional<USD> = 6493.912071090746 USD    # 0.6494 % of the base per ATR per leg
  }

  rules {
    # Session features, on the session calendar, from the minute bars.
    rel atr(A: Future, @S: session, V: Price<USD>)
    atr(A, S, V) :- session(A, S, _), V = mean(R) over (S1 in rows[atr_rows], true_range@session(A, S1, R)).

    # The opening prints, read at a decision bar (the session so far) or at a
    # session's close (the whole session): the same rule on either calendar.
    rel open0(A: Future, @X: base | session, O: Price<USD>)
    open0(A, X, O) :- O = first(P) over (T1 in session(X, [0min, 1min)), open(A, T1, P)).
    rel close_before[M: Duration](A: Future, @X: base | session, C: Price<USD>)
    close_before[M](A, X, C) :- C = last(P) over (T1 in session(X, [0min, M)), close(A, T1, P)).

    # m of a session, for the median; keyed at the session's close.
    rel session_m(A: Future, @S: session, M: Scalar)
    session_m(A, S, M) :- open0(A, S, O), close_before[30min](A, S, C), prev(S, S1), atr(A, S1, V),
        V > 0 USD/contract, M = (C - O) / V.
    rel m_median(A: Future, @S: session, Q: Scalar)
    m_median(A, S, Q) :- session(A, S, _), Q = median(abs(M)) over (S1 in rows[med_rows] min med_min, session_m(A, S1, M)).

    # At the decision bar: today's prints and the previous session's features
    # (a feature the previous session lacks is missing, not carried).
    rel signal(A: Future, @T, M: Scalar, G: Scalar, V: Price<USD>, Q: Scalar, O: Price<USD>, C20: Price<USD>)
    signal(A, T, M, G, V, Q, O, C20) :- universe(A, T),
        session_of(A, T, S), prev(S, S1),
        atr(A, S1, V), V > 0 USD/contract, m_median(A, S1, Q), close@session(A, S1, PC),
        open0(A, T, O), close_before[30min](A, T, C30), close_before[20min](A, T, C20),
        M = (C30 - O) / V, G = (O - PC) / V.

    rel aligned(A: Future, @T)
    aligned(A, T) :- signal(A, T, _, G, _, _, _, _), abs(G) < gap_free.
    aligned(A, T) :- signal(A, T, M, G, _, _, _, _), G * M > 0.

    # The morning is settled when the last ten minutes moved at most a third of the move so far.
    rel settled(A: Future, @T)
    settled(A, T) :- signal(A, T, M, _, V, _, O, C20), (O + M * V - C20) / (M * V) <= late_max.

    rel leg(A: Future, @T, Dir: Scalar, V: Price<USD>)
    leg(A, T, Dir, V) :- signal(A, T, M, G, V, Q, _, _),
        Dir = case { abs(M) >= k_main * Q, abs(M) > 0, aligned(A, T), settled(A, T) -> sign(M) ;
                     abs(M) <  k_main * Q, G <= side_gap                             -> -1 }.

    # Observed for the researcher (8.1): the plan reaches `aligned` and
    # `settled` only where a leg is being decided; observing them, and the
    # median that gates the legs, materialises their extension at every
    # decision bar for the ledger without changing a decision (Prop. 8.4).
    observe signal
    observe aligned trace instances
    observe settled trace instances
    observe m_median where A = "ES"
  }

  decisions {
    silence flat
    orders  moo_moc
    decide(T, hold(A, N)) :- leg(A, T, Dir, V), point_value(A, T, PV), N = Dir * risk / (V * PV).
  }

  # Fixed $1M, commission per contract only, fractional contracts.
  execution futures_fixed_base

  report {
    calendar es_days
    periods  252 per year
    window   2000-01-01 .. 2026-08-14
  }
}
