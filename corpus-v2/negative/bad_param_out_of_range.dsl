# expect: T
# A parameter's default must lie within its declared range: 400d is outside
# 5d..250d.
strategy bad_param_out_of_range {
  data { class equity   need bars @1d   need listing   require tested   prefer latest }
  uses calendar, stat, book
  clock { decide on base }
  universe { universe(A, T) :- listed(A, T), traded@base(A, T). }
  params { qty : Quantity<Shares> = 100 shares   lb : Duration = 400d in 5d..250d }
  rules {
    rel m(A: Equity, @T, M: Price<USD>)
    m(A, T, M) :- mean_window[lb, 1, close](A, T, M).
  }
  decisions {
    silence hold
    orders market
    decide(T, trade(A, qty)) :- m(A, T, _), flat(A, T).
  }
  execution equity_default
  report { calendar base   periods 252 per year }
}
