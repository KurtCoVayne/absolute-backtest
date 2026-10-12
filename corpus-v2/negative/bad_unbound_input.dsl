# expect: M
# `mean_window` takes a Duration in its first bracket, and N is bound by no
# literal of the rule (WF-2, Prop. 4.2): the family would be infinite. The
# diagnostic names N and the literal that needs it.
strategy bad_unbound_input {
  data { class equity   need bars @1d   need listing   require tested   prefer latest }
  uses calendar, stat, book
  clock { decide on base }
  universe { universe(A, T) :- listed(A, T), traded@base(A, T). }
  params { qty : Quantity<Shares> = 100 shares }
  rules {
    rel above(A: Equity, @T)
    above(A, T) :- close(A, T, P), mean_window[N, 10, close](A, T, M), P > M.
  }
  decisions {
    silence hold
    orders market
    decide(T, trade(A, qty)) :- above(A, T), flat(A, T).
  }
  execution equity_default
  report { calendar base   periods 252 per year }
}
