# expect: D
# A reduction without `by`: no total order, so which n tuples survive is
# unspecified (WF-7).
strategy bad_unordered_top {
  data { class equity   need bars @1d   need listing   require reviewed   prefer latest }
  uses calendar, book
  clock { decide on base }
  universe { universe(A, T) :- listed(A, T), traded@base(A, T). }
  params { n : Count = 3 }
  rules {
    rel candidate(A: Equity, @T, M: Scalar)
    candidate(A, T, M) :- universe(A, T), lag(T, 1mo, T1), close(A, T1, P1), lag(T, 6mo, T0), close(A, T0, P0), M = P1 / P0 - 1.
    rel selected(A: Equity, @T)
    selected(A, T) :- bar(T), top[n](candidate(A, T, M)) within (T).
  }
  decisions {
    silence hold
    orders market
    decide(T, hold(A, 0.1)) :- selected(A, T).
  }
  execution equity_default
  report { calendar base   periods 252 per year }
}
