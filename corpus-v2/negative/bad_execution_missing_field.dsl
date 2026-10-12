# expect: P
# An execution block written out must set every field (WF-14): `financing`,
# `leverage` and `numerics` are missing, and there is no default to fall back
# on, since the run is the program.
strategy bad_execution_missing_field {
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
  execution {
    price         close
    orders        market
    lot           whole
    capital       1_000_000 USD fixed
    commission    per_share 0.005 USD min 1 USD
    slippage      none
    impact        none
    participation none
    actions       apply
    delisting     last_price
    oversize      halt
    ruin          halt
  }
  report { calendar base   periods 252 per year }
}
