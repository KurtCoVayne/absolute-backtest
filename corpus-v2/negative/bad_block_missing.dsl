# expect: P
# Every block is present in a closed program (WF-14): the `report` block is
# missing, so the reporting calendar and the periods a year are unstated.
strategy bad_block_missing {
  data { class equity   need bars @1d   need listing   require tested   prefer latest }
  uses calendar, book
  clock { decide on base }
  universe { universe(A, T) :- listed(A, T), traded@base(A, T). }
  params { qty : Quantity<Shares> = 100 shares }
  rules { }
  decisions {
    silence hold
    orders market
    decide(T, trade(A, qty)) :- universe(A, T), flat(A, T).
  }
  execution equity_default
}
