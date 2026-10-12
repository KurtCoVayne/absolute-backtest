# expect: F
# `decided` must be read strictly before T; at the rule's own T the decision
# would see itself (WF-6 strictness).
strategy bad_same_time_history {
  data { class equity   need bars @1d   need listing   require tested   prefer latest }
  uses calendar, book
  clock { decide on base }
  universe { universe(A, T) :- listed(A, T), traded@base(A, T). }
  params { qty : Quantity<Shares> = 100 shares }
  rules {
    rel strong(A: Equity, @T)
    strong(A, T) :- universe(A, T), close(A, T, P), lag(T, 3mo, T0), close(A, T0, P0), P / P0 - 1 > 0.1.
  }
  decisions {
    silence hold
    orders market
    decide(T, trade(A, qty)) :- strong(A, T), flat(A, T), not decided(T, trade(A, _)).
  }
  execution equity_default
  report { calendar base   periods 252 per year }
}
