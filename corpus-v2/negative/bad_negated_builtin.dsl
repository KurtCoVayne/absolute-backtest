# expect: U
# `month_start` is a temporal builtin, not a relation, so `not month_start(T)`
# does not resolve. The idiom is a derived relation over the builtin,
# `mstart(T) :- bar(T), month_start(T).`, which is closed and can be negated.
strategy bad_negated_builtin {
  data { class equity   need bars @1d   need listing   require tested   prefer latest }
  uses calendar, book
  clock { decide on base }
  universe { universe(A, T) :- listed(A, T), traded@base(A, T). }
  params { qty : Quantity<Shares> = 100 shares }
  rules { }
  decisions {
    silence hold
    orders market
    decide(T, trade(A, qty)) :- universe(A, T), flat(A, T), not month_start(T).
    decide(T, trade(A, Q)) :- held(A, T, Q0), month_start(T), Q = -Q0.
  }
  execution equity_default
  report { calendar base   periods 252 per year }
}
