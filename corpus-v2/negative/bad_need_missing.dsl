# expect: E
# `split` is a primitive of the equity class that this demand did not ask
# for: the data block needs bars and listing, not actions. Add `need actions`.
strategy bad_need_missing {
  data { class equity   need bars @1d   need listing   require tested   prefer latest }
  uses calendar, book
  clock { decide on base }
  universe { universe(A, T) :- listed(A, T), traded@base(A, T). }
  params { qty : Quantity<Shares> = 100 shares }
  rules {
    rel split_today(A: Equity, @T)
    split_today(A, T) :- universe(A, T), split(A, T, _).
  }
  decisions {
    silence hold
    orders market
    decide(T, trade(A, Q)) :- split_today(A, T), held(A, T, Q0), Q = -Q0.
  }
  execution equity_default
  report { calendar base   periods 252 per year }
}
