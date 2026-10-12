# expect: K
# `level` recurses through `prev_row` with guards that are not complementary:
# when the close is above the carried level both the second and the third
# rule fire, two values at one key, so the relation is not keyed (WF-11).
# The keyed form is one rule with a case: `X = case { P > X0 -> P ; else -> X0 }`.
strategy bad_keyed_branching {
  data { class equity   need bars @1d   need listing   require tested   prefer latest }
  uses calendar, book
  clock { decide on base }
  universe { universe(A, T) :- listed(A, T), traded@base(A, T). }
  params { qty : Quantity<Shares> = 100 shares }
  rules {
    rel level(A: Equity, @T, X: Price<USD>)
    level(A, T, X) :- first_row(A, T), close(A, T, X).
    level(A, T, X) :- prev_row(A, T, T0), level(A, T0, X0), close(A, T, P), P > X0, X = P.
    level(A, T, X) :- prev_row(A, T, T0), level(A, T0, X0), X = X0.
  }
  decisions {
    silence hold
    orders market
    decide(T, trade(A, qty)) :- level(A, T, X), close(A, T, P), P >= X, flat(A, T).
  }
  execution equity_default
  report { calendar base   periods 252 per year }
}
