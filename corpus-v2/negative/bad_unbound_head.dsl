# expect: B
# The head variable X is bound by nothing in the body (WF-1).
strategy bad_unbound_head {
  data { class equity   need bars @1d   need listing   require tested   prefer latest }
  uses calendar, bars, book
  clock { decide on base }
  universe { universe(A, T) :- listed(A, T), traded@base(A, T). }
  params { qty : Quantity<Shares> = 100 shares }
  rules {
    rel signal(A: Equity, @T, X: Scalar)
    signal(A, T, X) :- log_price_return(A, T, R), R > 0.
  }
  decisions {
    silence hold
    orders market
    decide(T, trade(A, qty)) :- signal(A, T, X), X > 0.5, flat(A, T).
  }
  execution equity_default
  report { calendar base   periods 252 per year }
}
