# Scale into a trend one lot at a time: a first lot on the signal, another
# each time the close clears the last lot's price by `step`, at most
# `max_lots` lots over `span`. `lots_recent` and `last_lot_price` aggregate
# `lot_signal`'s own past, a cycle through an aggregate that the temporal
# stratification of WF-8 admits because `prior_window` steps strictly back
# in time; the first formulation rejected it (S), so a strategy counted its
# own lots through `decided` only.
strategy scale_in {
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
    threshold : Scalar = 0.05
    step      : Scalar = 0.03
    max_lots  : Count = 3
    span      : Duration = 60d
    hold      : Duration = 40d
    lot       : Quantity<Shares> = 100 shares
  }

  rules {
    rel momentum(A: Equity, @T, M: Scalar)
    momentum(A, T, M) :- universe(A, T), close(A, T, P1), lag(T, lookback, T0), close(A, T0, P0), M = P1 / P0 - 1.
    rel strong(A: Equity, @T)
    strong(A, T) :- momentum(A, T, M), M > threshold.

    # The lots signalled over the prior span, and the close at the last of them.
    rel lots_recent(A: Equity, @T, N: Count)
    lots_recent(A, T, N) :- universe(A, T), N = count(T0) over (T0 in prior_window[span] min 1, lot_signal(A, T0)).
    rel last_lot_price(A: Equity, @T, P: Price<USD>)
    last_lot_price(A, T, P) :- universe(A, T),
        P = last(P0) over (T0 in prior_window[span] min 1, lot_signal(A, T0), close(A, T0, P0)).

    # The first lot while flat; another while held, bought within `hold`, above the last lot by `step`.
    rel lot_signal(A: Equity, @T)
    lot_signal(A, T) :- strong(A, T), flat(A, T).
    lot_signal(A, T) :- strong(A, T), held(A, T, _), bought_within[hold](A, T),
        last_lot_price(A, T, P0), close(A, T, P), P > P0 * (1 + step),
        lots_recent(A, T, N), N < max_lots.
  }

  decisions {
    silence hold
    orders  market
    decide(T, trade(A, lot)) :- lot_signal(A, T).
    decide(T, trade(A, Q)) :- held(A, T, Q0), not bought_within[hold](A, T), Q = -Q0.
  }

  execution equity_default
  report { calendar base   periods 252 per year }
}
