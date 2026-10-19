# Dividend capture over the catalog: buy an index member once a dividend is
# announced and hold it through the ex-date. The announcement is read with
# the as-of join, so at T the strategy sees the latest announcement made at
# or before T and its ex-date as a value, never an announcement that had not
# yet been made.
strategy dividend_capture {
  data {
    class   equity
    need    bars @1d
    need    actions, listing
    need    lists "SPX"
    require reviewed
    prefer  latest
  }
  uses  calendar, book
  clock { decide on base }
  universe {
    universe(A, T) :- listed(A, T), traded@base(A, T).
  }

  params {
    qty : Quantity<Shares> = 100 shares
  }

  rules {
    # A dividend announced at or before T whose ex-date is still ahead.
    rel pending(A: Equity, @T)
    pending(A, T) :- universe(A, T), dividend(A, _, Ex, _, _) asof T, Ex > T.
  }

  decisions {
    silence hold
    orders  market
    decide(T, trade(A, qty)) :- universe(A, T), member(A, T, "SPX"), pending(A, T), flat(A, T).
    decide(T, trade(A, Q)) :- held(A, T, Q0), not pending(A, T), Q = -Q0.
  }

  execution equity_default
  report { calendar base   periods 252 per year }
}
