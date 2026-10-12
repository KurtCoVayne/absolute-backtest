# expect: T
# Price<USD> + Scalar: the dimensions do not balance (WF-3).
strategy bad_price_plus_scalar {
  data { class equity   need bars @1d   need listing   require tested   prefer latest }
  uses calendar, book
  clock { decide on base }
  universe { universe(A, T) :- listed(A, T), traded@base(A, T). }
  params { qty : Quantity<Shares> = 100 shares }
  rules {
    rel bumped(A: Equity, @T, X: Price<USD>)
    bumped(A, T, X) :- universe(A, T), close(A, T, P), X = P + 0.5.
  }
  decisions {
    silence hold
    orders market
    decide(T, trade(A, qty)) :- bumped(A, T, X), close(A, T, P), X > P, flat(A, T).
  }
  execution equity_default
  report { calendar base   periods 252 per year }
}
