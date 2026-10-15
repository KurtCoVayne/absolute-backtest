# Momentum entry with a trailing stop. The high-water mark since entry is the
# `state` module's `high_since_entry`: a keyed temporal recursion (WF-11)
# whose self-reference steps back through `prev`.
strategy trailing_stop {
  data {
    class   equity
    need    bars @1d
    need    actions, listing
    require tested
    prefer  latest
  }
  uses  calendar, state, book
  clock { decide on base }
  universe {
    universe(A, T) :- listed(A, T), traded@base(A, T).
  }

  params {
    lookback  : Duration = 3mo
    skip      : Duration = 0d
    threshold : Scalar = 0.1
    stop      : Scalar = 0.08 in 0.02..0.2
    qty       : Quantity<Shares> = 100 shares
  }

  rules {
    rel momentum(A: Equity, @T, M: Scalar)
    momentum(A, T, M) :- universe(A, T), lag(T, skip, T1), close(A, T1, P1),
        lag(T, lookback, T0), close(A, T0, P0), M = P1 / P0 - 1.

    rel strong(A: Equity, @T)
    strong(A, T) :- momentum(A, T, M), M > threshold.
  }

  decisions {
    silence hold
    orders  market
    decide(T, trade(A, qty)) :- strong(A, T), flat(A, T).
    decide(T, trade(A, Q)) :- held(A, T, Q0), high_since_entry(A, T, H), close(A, T, P), P < H * (1 - stop), Q = -Q0.
  }

  execution equity_default
  report { calendar base   periods 252 per year }
}
