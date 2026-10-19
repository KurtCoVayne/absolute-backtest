# Buy on a fast/slow moving-average cross up, sell on the cross down.
# `cross` is written positively: `not above(A, T0)` would be rejected by
# WF-5, since `above` reads `close`, which this demand does not require
# complete.
strategy sma_crossover {
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
    fast     : Duration = 20d in 10d..60d
    slow     : Duration = 50d in 30d..250d
    fast_min : Count = 12
    slow_min : Count = 30
    qty      : Quantity<Shares> = 100 shares
  }

  rules {
    rel above(A: Equity, @T)
    above(A, T) :- mean_window[fast, fast_min, close](A, T, F), mean_window[slow, slow_min, close](A, T, S), F > S.
    rel below(A: Equity, @T)
    below(A, T) :- mean_window[fast, fast_min, close](A, T, F), mean_window[slow, slow_min, close](A, T, S), F <= S.
  }

  decisions {
    silence hold
    orders  market
    decide(T, trade(A, qty)) :- above(A, T), prev(T, T0), below(A, T0), flat(A, T).
    decide(T, trade(A, Q)) :- below(A, T), prev(T, T0), above(A, T0), held(A, T, Q0), Q = -Q0.
  }

  execution equity_default
  report { calendar base   periods 252 per year }
}
