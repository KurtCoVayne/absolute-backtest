# expect: N
# `selected` includes a benchmark-dependent condition that reads `close`,
# which this demand does not require complete, so it is not closed:
# `not selected(A, T)` would let a missing benchmark close liquidate the book
# (WF-5). Negate `in_top`, a reduction, as regime_momentum does.
# allow: W6
strategy bad_negation_incomplete_derived {
  data { class equity   need bars @1d   need listing   require reviewed   prefer latest }
  uses calendar, stat, book
  clock { decide on base }
  universe { universe(A, T) :- listed(A, T), traded@base(A, T). }
  params { benchmark : Equity = "SPY"   n : Count = 3 }
  rules {
    rel regime_on(@T)
    regime_on(T) :- close(benchmark, T, P), mean_window[200d, 120, close](benchmark, T, M), P > M.
    rel candidate(A: Equity, @T, M: Scalar)
    candidate(A, T, M) :- universe(A, T), lag(T, 1mo, T1), close(A, T1, P1), lag(T, 6mo, T0), close(A, T0, P0), M = P1 / P0 - 1.
    rel in_top(A: Equity, @T)
    in_top(A, T) :- bar(T), top[n](candidate(A, T, M) by (M desc, A asc)) within (T).
    rel selected(A: Equity, @T)
    selected(A, T) :- regime_on(T), in_top(A, T).
    rel n_selected(@T, N: Count)
    n_selected(T, N) :- regime_on(T), N = count(A) over (in_top(A, T)) within (T), N > 0.
  }
  decisions {
    silence hold
    orders market
    decide(T, hold(A, W)) :- selected(A, T), n_selected(T, N), W = 1 / N.
    decide(T, hold(A, 0)) :- held(A, T, _), not selected(A, T).
  }
  execution equity_default
  report { calendar base   periods 252 per year }
}
