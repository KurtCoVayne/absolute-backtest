# expect: T
# `60 USD` is a Notional<USD> literal (currency), not a price: a Price<USD>
# parameter needs the per-share form `60 USD/share` (WF-3).
strategy bad_price_param_notional {
  env equities_1d
  uses features
  resolution @1d
  mode delta
  param floor : Price<USD> = 60 USD
  param qty : Quantity<Shares> = 100 shares
  rel pricey(-A: Equity, @T: Timestamp)
  pricey(A, T) :- universe(A, T), close(A, T, P), P > floor.
  decide(T, buy(A, qty)) :- pricey(A, T), flat(A, T).
}
