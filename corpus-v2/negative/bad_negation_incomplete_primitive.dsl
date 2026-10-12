# expect: N
# `close` is not closed under this demand: a missing price is unknown, not
# false, so `not close(A, T, _)` could fire on a data gap (WF-5). With
# `need bars @1d complete` the demand would require a reviewed store, where a
# missing bar means the name did not trade, and the negation would be admitted.
strategy bad_negation_incomplete_primitive {
  data { class equity   need bars @1d   need listing   require tested   prefer latest }
  uses calendar, book
  clock { decide on base }
  universe { universe(A, T) :- listed(A, T). }
  params { }
  rules {
    rel halted(A: Equity, @T)
    halted(A, T) :- universe(A, T), not close(A, T, _).
  }
  decisions {
    silence hold
    orders market
    decide(T, trade(A, Q)) :- halted(A, T), held(A, T, Q0), Q = -Q0.
  }
  execution equity_default
  report { calendar base   periods 252 per year }
}
