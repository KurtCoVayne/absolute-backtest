# expect: V
# `"SP1500"` occurs in a rule but in no `need lists` of the data block
# (WF-15); the S&P 1500 is written as the union of the three index lists the
# catalog holds, as corpus-v2/company/mw14.dsl does.
strategy bad_unknown_label {
  data { class equity   need bars @1d   need listing   need lists "SP500"   require tested   prefer latest }
  uses calendar, book
  clock { decide on base }
  universe { universe(A, T) :- listed(A, T), traded@base(A, T). }
  params { qty : Quantity<Shares> = 100 shares }
  rules {
    rel eligible(A: Equity, @T)
    eligible(A, T) :- universe(A, T), member(A, T, "SP1500").
  }
  decisions {
    silence hold
    orders market
    decide(T, trade(A, qty)) :- eligible(A, T), flat(A, T).
  }
  execution equity_default
  report { calendar base   periods 252 per year }
}
