# expect: X
# The rule is on the daily decision calendar and `close` is at the minute
# base: a base relation is read at a daily label only through its bar view,
# `close@daily` (WF-16).
strategy bad_calendar_mix {
  data { class equity   need bars @1m   need listing   require reviewed   prefer latest }
  uses calendar, book
  calendars { daily = days }
  clock { decide on daily }
  universe { universe(A, T) :- listed(A, T), traded@daily(A, T). }
  params { n : Count = 2 }
  rules {
    rel last_price(A: Equity, @T, P: Price<USD>)
    last_price(A, T, P) :- universe(A, T), close(A, T, P).
    rel chosen(A: Equity, @T)
    chosen(A, T) :- bar(T), top[n](last_price(A, T, P) by (P desc, A asc)) within (T).
  }
  decisions {
    silence hold
    orders market
    decide(T, hold(A, 0.5)) :- chosen(A, T).
  }
  execution equity_default
  report { calendar daily   periods 252 per year }
}
