# Buy names whose momentum beats the benchmark's by a margin. The exit is the
# point form of a time-based exit: sell at the bar exactly `hold` after a buy.
# allow: W6  (the ticker literal is a snapshot of the bundle date)
strategy relative_strength {
  data {
    class   equity
    need    bars @1d
    need    actions, listing
    require tested
    prefer  latest
  }
  uses  calendar, book
  clock { decide on base }
  universe {
    universe(A, T) :- listed(A, T), traded@base(A, T).
  }

  params {
    benchmark : Equity = "SPY"
    lookback  : Duration = 3mo
    skip      : Duration = 0d
    margin    : Scalar = 0.05
    hold      : Duration = 20d
    qty       : Quantity<Shares> = 100 shares
  }

  rules {
    rel momentum(A: Equity, @T, M: Scalar)
    momentum(A, T, M) :- universe(A, T), lag(T, skip, T1), close(A, T1, P1),
        lag(T, lookback, T0), close(A, T0, P0), M = P1 / P0 - 1.

    rel bench_mom(@T, M: Scalar)
    bench_mom(T, M) :- bar(T), momentum(benchmark, T, M).

    rel outperformer(A: Equity, @T)
    outperformer(A, T) :- momentum(A, T, M), bench_mom(T, B), M > B + margin.
  }

  decisions {
    silence hold
    orders  market
    decide(T, trade(A, qty)) :- outperformer(A, T), flat(A, T), not bought_within[hold](A, T).
    decide(T, trade(A, Q)) :- held(A, T, Q0), lag(T, hold, T0), decided(T0, trade(A, Q1)), Q1 > 0 shares, Q = -Q0.
  }

  execution equity_default
  report { calendar base   periods 252 per year }
}
