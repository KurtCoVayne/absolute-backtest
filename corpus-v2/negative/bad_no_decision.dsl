# expect: Z
# A decisions block with no decide rule produces nothing: at least one decide
# rule is required (the one judgment of WF-9 the second formulation keeps).
strategy bad_no_decision {
  data { class equity   need bars @1d   need listing   require reviewed   prefer latest }
  uses calendar, book
  clock { decide on base }
  universe { universe(A, T) :- listed(A, T), traded@base(A, T). }
  params { }
  rules {
    rel strong(A: Equity, @T)
    strong(A, T) :- universe(A, T), close(A, T, P), lag(T, 3mo, T0), close(A, T0, P0), P / P0 - 1 > 0.1.
  }
  decisions {
    silence hold
    orders market
  }
  execution equity_default
  report { calendar base   periods 252 per year }
}
