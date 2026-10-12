# expect: F
# `close(A, T1, P1)` binds its temporal key to a fresh T1 that is not derived
# from T; the comparison T1 > T afterwards makes it a forward return (WF-6).
strategy bad_lookahead {
  data { class equity   need bars @1d   need listing   require tested   prefer latest }
  uses calendar, book
  clock { decide on base }
  universe { universe(A, T) :- listed(A, T), traded@base(A, T). }
  params { qty : Quantity<Shares> = 100 shares }
  rules {
    rel fwd(A: Equity, @T, R: Scalar)
    fwd(A, T, R) :- universe(A, T), close(A, T, P0), close(A, T1, P1), T1 > T, R = P1 / P0 - 1.
  }
  decisions {
    silence hold
    orders market
    decide(T, trade(A, qty)) :- fwd(A, T, R), R > 0.02, flat(A, T).
  }
  execution equity_default
  report { calendar base   periods 252 per year }
}
