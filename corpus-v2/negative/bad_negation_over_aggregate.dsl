# expect: N
# Atoms inside an aggregation's conjunction are positive atoms of the rule
# for WF-5: `mean_close` aggregates over `close`, which is not closed under
# this demand, so the relation is not closed even though `count` fires on an
# empty group.
strategy bad_negation_over_aggregate {
  data { class equity   need bars @1d   need listing   require tested   prefer latest }
  uses calendar, book
  clock { decide on base }
  universe { universe(A, T) :- listed(A, T), traded@base(A, T). }
  params { qty : Quantity<Shares> = 100 shares }
  rules {
    rel mean_close(A: Equity, @T, M: Price<USD>)
    mean_close(A, T, M) :- universe(A, T), M = mean(P) over (T1 in window[5d] min 1, close(A, T1, P)).
    rel n_close(A: Equity, @T, N: Count)
    n_close(A, T, N) :- universe(A, T), N = count(P) over (T1 in window[5d] min 1, close(A, T1, P)).
  }
  decisions {
    silence hold
    orders market
    decide(T, trade(A, Q)) :- held(A, T, Q0), not mean_close(A, T, _), Q = -Q0.
    decide(T, trade(A, qty)) :- universe(A, T), flat(A, T), not n_close(A, T, _).
  }
  execution equity_default
  report { calendar base   periods 252 per year }
}
