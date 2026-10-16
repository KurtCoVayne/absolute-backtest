# expect: U
# `rsi` is declared nowhere: not in the strategy, its libraries or the class.
strategy bad_undeclared {
  data { class equity   need bars @1d   need listing   require reviewed   prefer latest }
  uses calendar, book
  clock { decide on base }
  universe { universe(A, T) :- listed(A, T), traded@base(A, T). }
  params { qty : Quantity<Shares> = 100 shares }
  rules { }
  decisions {
    silence hold
    orders market
    decide(T, trade(A, qty)) :- universe(A, T), rsi[14](A, T, R), R < 30, flat(A, T).
  }
  execution equity_default
  report { calendar base   periods 252 per year }
}
