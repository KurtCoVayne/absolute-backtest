# Hold names whose 20-day average close is above a price floor and exit when
# it falls back below. The floor is a `Price<USD>` parameter written with the
# price literal `60 USD/share` (section 2: currency per share), so it can be
# compared directly with the Price-valued `sma` feature.
strategy price_floor {
  env equities_1d
  uses features
  resolution @1d
  mode delta

  param floor : Price<USD> = 60 USD/share in 10 USD/share..500 USD/share
  param lookback : Duration = 20d in 5d..60d
  param lookback_min : Count = 12
  param qty : Quantity<Shares> = 100 shares

  rel pricey(-A: Equity, @T: Timestamp)
  pricey(A, T) :- universe(A, T), sma(A, T, lookback, lookback_min, M), M > floor.

  rel cheap(-A: Equity, @T: Timestamp)
  cheap(A, T) :- universe(A, T), sma(A, T, lookback, lookback_min, M), M <= floor.

  decide(T, buy(A, qty)) :- pricey(A, T), flat(A, T).
  decide(T, sell(A, Q)) :- cheap(A, T), held(A, T, Q).
}
