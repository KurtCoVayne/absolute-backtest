# expect: T
# `60 USD` is a Notional<USD> literal (currency), not a price: a Price<USD>
# parameter needs the per-share form `60 USD/share` (WF-3).
strategy bad_price_param_notional {
  data { class equity   need bars @1d   need listing   require tested   prefer latest }
  uses calendar, book
  clock { decide on base }
  universe { universe(A, T) :- listed(A, T), traded@base(A, T). }
  params { floor : Price<USD> = 60 USD   qty : Quantity<Shares> = 100 shares }
  rules {
    rel pricey(A: Equity, @T)
    pricey(A, T) :- universe(A, T), close(A, T, P), P > floor.
  }
  decisions {
    silence hold
    orders market
    decide(T, trade(A, qty)) :- pricey(A, T), flat(A, T).
  }
  execution equity_default
  report { calendar base   periods 252 per year }
}
