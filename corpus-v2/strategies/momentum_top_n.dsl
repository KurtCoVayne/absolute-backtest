# Equal-weight the top-n momentum names that clear a dollar-volume floor.
# `selected` is a reduction, so it is closed and `not selected` is legal.
strategy momentum_top_n {
  data {
    class   equity
    need    bars @1d
    need    actions, listing
    require tested
    prefer  latest
  }
  uses  calendar, bars, book
  clock { decide on base }
  universe {
    universe(A, T) :- listed(A, T), traded@base(A, T).
  }

  params {
    lookback : Duration = 1y in 6mo..2y
    skip     : Duration = 1mo in 0d..3mo
    min_adv  : Notional<USD> = 1_000_000 USD
    n        : Count = 3 in 1..50
  }

  rules {
    rel momentum(A: Equity, @T, M: Scalar)
    momentum(A, T, M) :- universe(A, T), lag(T, skip, T1), close(A, T1, P1),
        lag(T, lookback, T0), close(A, T0, P0), M = P1 / P0 - 1.

    rel candidate(A: Equity, @T, M: Scalar)
    candidate(A, T, M) :- liquid[min_adv](A, T), momentum(A, T, M).

    rel selected(A: Equity, @T)
    selected(A, T) :- bar(T), top[n](candidate(A, T, M) by (M desc, A asc)) within (T).

    rel n_selected(@T, N: Count)
    n_selected(T, N) :- bar(T), N = count(A) over (selected(A, T)) within (T), N > 0.
  }

  decisions {
    silence hold
    orders  market
    decide(T, hold(A, W)) :- selected(A, T), n_selected(T, N), W = 1 / N.
    decide(T, hold(A, 0)) :- held(A, T, _), not selected(A, T).
  }

  execution equity_default
  report { calendar base   periods 252 per year }
}
