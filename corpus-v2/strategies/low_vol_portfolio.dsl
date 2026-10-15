# Monthly: hold the n lowest-volatility names, weighted by inverse volatility.
strategy low_vol_portfolio {
  data {
    class   equity
    need    bars @1d
    need    actions, listing
    require tested
    prefer  latest
  }
  uses  calendar, stat, bars, book
  clock { decide on base }
  universe {
    universe(A, T) :- listed(A, T), traded@base(A, T).
  }

  params {
    lb     : Duration = 60d in 20d..250d
    lb_min : Count = 40
    n      : Count = 3
  }

  rules {
    rel vol(A: Equity, @T, V: Scalar)
    vol(A, T, V) :- std_window[lb, lb_min, log_price_return](A, T, V), V > 0.

    rel chosen(A: Equity, @T, V: Scalar)
    chosen(A, T, V) :- bar(T), month_start(T), top[n](vol(A, T, V) by (V asc, A asc)) within (T).

    rel inv_sum(@T, S: Scalar)
    inv_sum(T, S) :- bar(T), S = sum(1 / V) over (chosen(_, T, V)) within (T).
  }

  decisions {
    silence hold
    orders  market
    decide(T, hold(A, W)) :- chosen(A, T, V), inv_sum(T, S), W = (1 / V) / S.
    decide(T, hold(A, 0)) :- bar(T), month_start(T), held(A, T, _), not chosen(A, T, _).
  }

  execution equity_default
  report { calendar base   periods 252 per year }
}
