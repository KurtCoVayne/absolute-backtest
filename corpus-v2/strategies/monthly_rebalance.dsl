# Equal-weight top-n momentum, rebalanced on the first bar of each month.
strategy monthly_rebalance {
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
    lookback : Duration = 6mo in 3mo..1y
    skip     : Duration = 1mo
    n        : Count = 3
  }

  rules {
    rel rebalance(@T)
    rebalance(T) :- bar(T), month_start(T).

    rel momentum(A: Equity, @T, M: Scalar)
    momentum(A, T, M) :- universe(A, T), lag(T, skip, T1), close(A, T1, P1),
        lag(T, lookback, T0), close(A, T0, P0), M = P1 / P0 - 1.

    rel candidate(A: Equity, @T, M: Scalar)
    candidate(A, T, M) :- rebalance(T), momentum(A, T, M).

    rel chosen(A: Equity, @T)
    chosen(A, T) :- rebalance(T), top[n](candidate(A, T, M) by (M desc, A asc)) within (T).

    rel n_chosen(@T, N: Count)
    n_chosen(T, N) :- rebalance(T), N = count(A) over (chosen(A, T)) within (T), N > 0.
  }

  decisions {
    silence hold
    orders  market
    decide(T, hold(A, W)) :- chosen(A, T), n_chosen(T, N), W = 1 / N.
    decide(T, hold(A, 0)) :- rebalance(T), held(A, T, _), not chosen(A, T).
  }

  execution equity_default
  report { calendar base   periods 252 per year }
}
