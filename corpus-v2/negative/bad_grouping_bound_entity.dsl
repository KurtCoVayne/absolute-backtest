# expect: G
# `A` is bound by `universe(A, T)` before the rank and is not in `within (T)`:
# a rank that varies over A must not bind it (WF-13). Either rank one tuple,
# `within (A, T)`, or drop the binding literal and let the reduction bind A,
# as `liquid` in corpus-v2/company/mw14.dsl does.
strategy bad_grouping_bound_entity {
  data { class equity   need bars @1d   need listing   require reviewed   prefer latest }
  uses calendar, bars, book
  clock { decide on base }
  universe { universe(A, T) :- listed(A, T), traded@base(A, T). }
  params { qty : Quantity<Shares> = 100 shares   cut : Count = 5 }
  rules {
    rel liquid(A: Equity, @T)
    liquid(A, T) :- universe(A, T),
        K = rank_average(dollar_volume(A, T, D) by (D asc)) within (T),
        N = count(A1) over (dollar_volume(A1, T, _)) within (T),
        decile(K, N) >= cut.
  }
  decisions {
    silence hold
    orders market
    decide(T, trade(A, qty)) :- liquid(A, T), flat(A, T).
  }
  execution equity_default
  report { calendar base   periods 252 per year }
}
