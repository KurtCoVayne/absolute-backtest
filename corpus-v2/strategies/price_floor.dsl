# Hold names whose 20-day average close is above a price floor and exit when
# it falls back below. The floor is a `Price<USD>` parameter written with the
# price literal `60 USD/share` (currency per share), so it compares directly
# with the Price-valued rolling mean.
strategy price_floor {
  data {
    class   equity
    need    bars @1d
    need    actions, listing
    require reviewed
    prefer  latest
  }
  uses  calendar, stat, book
  clock { decide on base }
  universe {
    universe(A, T) :- listed(A, T), traded@base(A, T).
  }

  params {
    floor        : Price<USD> = 60 USD/share in 10 USD/share..500 USD/share
    lookback     : Duration = 20d in 5d..60d
    lookback_min : Count = 12
    qty          : Quantity<Shares> = 100 shares
  }

  rules {
    rel pricey(A: Equity, @T)
    pricey(A, T) :- mean_window[lookback, lookback_min, close](A, T, M), M > floor.
    rel cheap(A: Equity, @T)
    cheap(A, T) :- mean_window[lookback, lookback_min, close](A, T, M), M <= floor.
  }

  decisions {
    silence hold
    orders  market
    decide(T, trade(A, qty)) :- pricey(A, T), flat(A, T).
    decide(T, trade(A, Q)) :- cheap(A, T), held(A, T, Q0), Q = -Q0.
  }

  execution equity_default
  report { calendar base   periods 252 per year }
}
