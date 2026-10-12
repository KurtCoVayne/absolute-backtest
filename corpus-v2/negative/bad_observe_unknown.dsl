# expect: O
# `observe` names a relation of the program; `rsi` is declared nowhere.
strategy bad_observe_unknown {
  data { class equity   need bars @1d   need listing   require tested   prefer latest }
  uses calendar, bars, book
  clock { decide on base }
  universe { universe(A, T) :- listed(A, T), traded@base(A, T). }
  params { qty : Quantity<Shares> = 100 shares }
  rules {
    rel up(A: Equity, @T)
    up(A, T) :- log_price_return(A, T, R), R > 0.
    observe rsi
  }
  decisions {
    silence hold
    orders market
    decide(T, trade(A, qty)) :- up(A, T), flat(A, T).
  }
  execution equity_default
  report { calendar base   periods 252 per year }
}
