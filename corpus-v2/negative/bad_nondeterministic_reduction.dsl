# expect: D
# `first` and `last` are ordered by the temporal key of a rows, window or
# session constraint; over the entities of one bar the group has no order, so
# which price is first is unspecified (WF-7).
strategy bad_nondeterministic_reduction {
  data { class equity   need bars @1d   need listing   require tested   prefer latest }
  uses calendar, book
  clock { decide on base }
  universe { universe(A, T) :- listed(A, T), traded@base(A, T). }
  params { qty : Quantity<Shares> = 100 shares }
  rules {
    rel ref_price(@T, X: Price<USD>)
    ref_price(T, X) :- bar(T), X = first(P) over (close(_, T, P)) within (T).
  }
  decisions {
    silence hold
    orders market
    decide(T, trade(A, qty)) :- ref_price(T, X), close(A, T, P), P > X, flat(A, T).
  }
  execution equity_default
  report { calendar base   periods 252 per year }
}
