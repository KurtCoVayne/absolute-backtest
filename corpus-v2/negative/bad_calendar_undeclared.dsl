# expect: X
# `@T: weekly` names a calendar the `calendars` block does not declare and
# the class does not provide (WF-16).
strategy bad_calendar_undeclared {
  data { class equity   need bars @1d   need listing   require tested   prefer latest }
  uses calendar, book
  clock { decide on base }
  universe { universe(A, T) :- listed(A, T), traded@base(A, T). }
  params { qty : Quantity<Shares> = 100 shares }
  rules {
    rel wk_close(A: Equity, @T: weekly, P: Price<USD>)
    wk_close(A, T, P) :- close@weekly(A, T, P).
  }
  decisions {
    silence hold
    orders market
    decide(T, trade(A, qty)) :- universe(A, T), flat(A, T), wk_close(A, T, _).
  }
  execution equity_default
  report { calendar base   periods 252 per year }
}
