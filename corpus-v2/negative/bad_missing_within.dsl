# expect: G
# A rank names its group in `within (...)`; without it the grouping is the
# silent one of the first formulation, which made every rank 1 in MW14 (R6).
strategy bad_missing_within {
  data { class equity   need bars @1d   need listing   require reviewed   prefer latest }
  uses calendar, bars, book
  clock { decide on base }
  universe { universe(A, T) :- listed(A, T), traded@base(A, T). }
  params { qty : Quantity<Shares> = 100 shares   n_enter : Count = 7 }
  rules {
    rel ranked(A: Equity, @T, K: Count)
    ranked(A, T, K) :- K = rank(dollar_volume(A, T, D) by (D desc, A asc)).
  }
  decisions {
    silence hold
    orders market
    decide(T, trade(A, qty)) :- ranked(A, T, K), K <= n_enter, flat(A, T).
  }
  execution equity_default
  report { calendar base   periods 252 per year }
}
