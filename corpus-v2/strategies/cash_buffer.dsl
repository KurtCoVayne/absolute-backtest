# Momentum entries that are only taken when cash after the order keeps a
# buffer; the strategy observes the executor's cash, not an intended cash.
# The time-based exit is the window form (see breakout_52w).
strategy cash_buffer {
  data {
    class   equity
    need    bars @1d
    need    actions, listing
    require reviewed
    prefer  latest
  }
  uses  calendar, book
  clock { decide on base }
  universe {
    universe(A, T) :- listed(A, T), traded@base(A, T).
  }

  params {
    lookback  : Duration = 3mo
    skip      : Duration = 0d
    threshold : Scalar = 0.05
    qty       : Quantity<Shares> = 100 shares
    buffer    : Notional<USD> = 10_000 USD
    hold      : Duration = 30d
  }

  rules {
    # The return from `lookback` ago to `skip` ago; 0d means to the latest close.
    rel momentum(A: Equity, @T, M: Scalar)
    momentum(A, T, M) :- universe(A, T), lag(T, skip, T1), close(A, T1, P1),
        lag(T, lookback, T0), close(A, T0, P0), M = P1 / P0 - 1.

    rel affordable(A: Equity, @T)
    affordable(A, T) :- close(A, T, P), cash(T, C), Cost = P * qty, Cost <= C - buffer.

    rel entry_signal(A: Equity, @T)
    entry_signal(A, T) :- momentum(A, T, M), M > threshold.
  }

  decisions {
    silence hold
    orders  market
    decide(T, trade(A, qty)) :- entry_signal(A, T), flat(A, T), affordable(A, T).
    decide(T, trade(A, Q)) :- held(A, T, Q0), not bought_within[hold](A, T), Q = -Q0.
  }

  execution equity_default
  report { calendar base   periods 252 per year }
}
