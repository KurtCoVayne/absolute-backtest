# expect: S
# `a` and `b` negate each other at the same T: a cycle through negation that
# does not step back in time has no stratification (WF-8, temporal
# stratification admits only cycles every path of which steps strictly back).
strategy bad_negation_cycle {
  data { class equity   need bars @1d   need listing   require reviewed   prefer latest }
  uses calendar, book
  clock { decide on base }
  universe { universe(A, T) :- listed(A, T), traded@base(A, T). }
  params { qty : Quantity<Shares> = 100 shares }
  rules {
    rel a(A: Equity, @T)
    a(A, T) :- universe(A, T), not b(A, T).
    rel b(A: Equity, @T)
    b(A, T) :- universe(A, T), not a(A, T).
  }
  decisions {
    silence hold
    orders market
    decide(T, trade(A, qty)) :- a(A, T), flat(A, T).
  }
  execution equity_default
  report { calendar base   periods 252 per year }
}
