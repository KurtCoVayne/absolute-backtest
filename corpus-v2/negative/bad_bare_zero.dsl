# expect: T
# `V > 0` compares a Price<USD> with a bare Scalar; the diagnostic names the
# typed zero to write, `0 USD/share` (7.3).
strategy bad_bare_zero {
  data { class equity   need bars @1d   need listing   require reviewed   prefer latest }
  uses calendar, bars, book
  clock { decide on base }
  universe { universe(A, T) :- listed(A, T), traded@base(A, T). }
  params { qty : Quantity<Shares> = 100 shares }
  rules {
    rel wide(A: Equity, @T)
    wide(A, T) :- true_range(A, T, V), V > 0, close(A, T, P), V / P > 0.05.
  }
  decisions {
    silence hold
    orders market
    decide(T, trade(A, qty)) :- wide(A, T), flat(A, T).
  }
  execution equity_default
  report { calendar base   periods 252 per year }
}
