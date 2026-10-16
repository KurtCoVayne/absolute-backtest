# expect: U
# `prev` is a temporal builtin; a relation may not take the name of a
# builtin, a block or a keyword (`window`, `rows`, `session` included), and
# the rules that mention it are not judged.
strategy bad_reserved_name {
  data { class equity   need bars @1d   need listing   require reviewed   prefer latest }
  uses calendar, book
  clock { decide on base }
  universe { universe(A, T) :- listed(A, T), traded@base(A, T). }
  params { qty : Quantity<Shares> = 100 shares }
  rules {
    rel prev(A: Equity, @T)
    prev(A, T) :- universe(A, T).
  }
  decisions {
    silence hold
    orders market
    decide(T, trade(A, qty)) :- prev(A, T), flat(A, T).
  }
  execution equity_default
  report { calendar base   periods 252 per year }
}
