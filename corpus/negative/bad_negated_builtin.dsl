# expect: U
# `month_start` is a temporal builtin, not a relation, so `not month_start(T)`
# does not resolve (section 4). The idiom is a derived relation over the
# builtin, `mstart(T) :- bar(T), month_start(T).`, which is complete and can
# be negated.
strategy bad_negated_builtin {
  env equities_1d
  uses features
  resolution @1d
  mode delta
  param qty : Quantity<Shares> = 100 shares
  decide(T, buy(A, qty)) :- universe(A, T), flat(A, T), not month_start(T).
  decide(T, sell(A, Q)) :- held(A, T, Q), month_start(T).
}
