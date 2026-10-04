# Momentum entries that are only taken when cash after the order keeps a
# buffer; the strategy observes the executor's cash, not an intended cash.
# The time-based exit is the window form (see breakout_52w).
strategy cash_buffer {
  env equities_1d
  uses features
  resolution @1d
  mode delta

  param lookback : Duration = 3mo
  param skip : Duration = 0d
  param threshold : Scalar = 0.05
  param qty : Quantity<Shares> = 100 shares
  param buffer : Notional<USD> = 10_000 USD
  param hold : Duration = 30d

  rel affordable(-A: Equity, @T: Timestamp)
  affordable(A, T) :- universe(A, T), close(A, T, P), cash(T, C), Cost = P * qty, Cost <= C - buffer.

  rel entry_signal(-A: Equity, @T: Timestamp)
  entry_signal(A, T) :- universe(A, T), momentum(A, T, lookback, skip, M), M > threshold.

  decide(T, buy(A, qty)) :- entry_signal(A, T), flat(A, T), affordable(A, T).
  decide(T, sell(A, Q)) :- held(A, T, Q), not bought_within(A, T, hold).
}
