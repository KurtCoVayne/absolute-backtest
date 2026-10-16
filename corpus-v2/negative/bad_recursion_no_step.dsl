# expect: R
# `trend` refers to itself at the same T: a cycle with no strictly earlier
# step through prev, prev_row or lag (WF-4).
strategy bad_recursion_no_step {
  data { class equity   need bars @1d   need listing   require reviewed   prefer latest }
  uses calendar, stat, book
  clock { decide on base }
  universe { universe(A, T) :- listed(A, T), traded@base(A, T). }
  params { qty : Quantity<Shares> = 100 shares }
  rules {
    rel trend(A: Equity, @T)
    trend(A, T) :- close(A, T, P), mean_window[50d, 30, close](A, T, M), P > M.
    trend(A, T) :- universe(A, T), trend(A, T).
  }
  decisions {
    silence hold
    orders market
    decide(T, trade(A, qty)) :- trend(A, T), flat(A, T).
  }
  execution equity_default
  report { calendar base   periods 252 per year }
}
