# Buy names whose momentum beats the benchmark's by a margin.
# allow: W6  (the ticker literal is a snapshot of the bundle date)
strategy relative_strength {
  env equities_1d
  uses features
  resolution @1d
  mode delta

  param benchmark : Equity = "SPY"
  param lookback : Duration = 3mo
  param skip : Duration = 0d
  param margin : Scalar = 0.05
  param hold : Duration = 20d
  param qty : Quantity<Shares> = 100 shares

  rel bench_mom(@T: Timestamp, -M: Scalar)
  bench_mom(T, M) :- bar(T), momentum(benchmark, T, lookback, skip, M).

  rel outperformer(-A: Equity, @T: Timestamp)
  outperformer(A, T) :- universe(A, T), momentum(A, T, lookback, skip, M), bench_mom(T, B), M > B + margin.

  decide(T, buy(A, qty)) :- outperformer(A, T), flat(A, T), not bought_within(A, T, hold).
  decide(T, sell(A, Q)) :- held(A, T, Q), lag(T, hold, T0), decided(T0, buy(A, _)).
}
