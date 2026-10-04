# expect: N
# Atoms inside an aggregation's conjunction are positive atoms of the rule
# for WF-5: `mean_close` aggregates over `close`, which is not complete, so
# the relation is not complete even though `count` fires on an empty group.
strategy bad_negation_over_aggregate {
  env equities_1d
  uses features
  resolution @1d
  mode delta

  param qty : Quantity<Shares> = 100 shares

  rel mean_close(-A: Equity, @T: Timestamp, -M: Price<USD>)
  mean_close(A, T, M) :- universe(A, T), M = mean(P) over (T1 in window(T, 5d, min 1), close(A, T1, P)).

  rel n_close(-A: Equity, @T: Timestamp, -N: Count)
  n_close(A, T, N) :- universe(A, T), N = count(P) over (T1 in window(T, 5d, min 1), close(A, T1, P)).

  decide(T, sell(A, Q)) :- held(A, T, Q), not mean_close(A, T, _).
  decide(T, buy(A, qty)) :- universe(A, T), flat(A, T), not n_close(A, T, _).
}
