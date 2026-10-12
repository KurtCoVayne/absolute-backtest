# Momentum on total return over the catalog: the top names by compounded
# total return, liquid, index members, and never a delisted one (a delisted
# name has no rows after its last bar).
strategy total_return_momentum {
  data {
    class   equity
    need    bars @1d
    need    actions, listing
    need    lists "SPX"
    require tested
    prefer  latest
  }
  uses  calendar, catalog, stat, bars, book
  clock { decide on base }
  universe {
    universe(A, T) :- listed(A, T), traded@base(A, T).
  }

  params {
    lookback : Duration = 6mo in 3mo..1y
    min_obs  : Count = 60
    min_adv  : Notional<USD> = 1_000_000 USD
    n        : Count = 2 in 1..20
  }

  rules {
    rel eligible(A: Equity, @T)
    eligible(A, T) :- universe(A, T), member(A, T, "SPX"), liquid[min_adv](A, T).

    rel candidate(A: Equity, @T, M: Scalar)
    candidate(A, T, M) :- eligible(A, T), sum_window[lookback, min_obs, log_return](A, T, M).

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
