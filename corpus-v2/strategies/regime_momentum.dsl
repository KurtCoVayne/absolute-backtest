# Momentum top-n while the benchmark trades above its trend; flat otherwise.
# WF-5's teeth: `selected` includes the benchmark condition, so it is not
# closed and `not selected(A, T)` is rejected; a missing benchmark close
# cannot liquidate the book. Liquidation negates `in_top`, a reduction.
# allow: W6  (the ticker literal is a snapshot of the bundle date)
strategy regime_momentum {
  data {
    class   equity
    need    bars @1d
    need    actions, listing
    require reviewed
    prefer  latest
  }
  uses  calendar, stat, bars, book
  clock { decide on base }
  universe {
    universe(A, T) :- listed(A, T), traded@base(A, T).
  }

  params {
    benchmark : Equity = "SPY"
    trend     : Duration = 200d in 100d..300d
    trend_min : Count = 120
    lookback  : Duration = 6mo
    skip      : Duration = 1mo
    min_adv   : Notional<USD> = 1_000_000 USD
    n         : Count = 3
  }

  rules {
    rel regime_on(@T)
    regime_on(T) :- close(benchmark, T, P), mean_window[trend, trend_min, close](benchmark, T, M), P > M.
    rel regime_off(@T)
    regime_off(T) :- close(benchmark, T, P), mean_window[trend, trend_min, close](benchmark, T, M), P <= M.

    rel momentum(A: Equity, @T, M: Scalar)
    momentum(A, T, M) :- universe(A, T), lag(T, skip, T1), close(A, T1, P1),
        lag(T, lookback, T0), close(A, T0, P0), M = P1 / P0 - 1.

    rel candidate(A: Equity, @T, M: Scalar)
    candidate(A, T, M) :- liquid[min_adv](A, T), momentum(A, T, M).

    rel in_top(A: Equity, @T)
    in_top(A, T) :- bar(T), top[n](candidate(A, T, M) by (M desc, A asc)) within (T).

    rel selected(A: Equity, @T)
    selected(A, T) :- regime_on(T), in_top(A, T).

    rel n_selected(@T, N: Count)
    n_selected(T, N) :- regime_on(T), N = count(A) over (in_top(A, T)) within (T), N > 0.
  }

  decisions {
    silence hold
    orders  market
    decide(T, hold(A, W)) :- selected(A, T), n_selected(T, N), W = 1 / N.
    decide(T, hold(A, 0)) :- regime_on(T), held(A, T, _), not in_top(A, T).
    decide(T, hold(A, 0)) :- regime_off(T), held(A, T, _).
  }

  execution equity_default
  report { calendar base   periods 252 per year }
}
