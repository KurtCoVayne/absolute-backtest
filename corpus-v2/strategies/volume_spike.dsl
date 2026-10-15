# Buy a volume spike on an up day; sell at the first bar more than `hold`
# after the entry (the window form of a time-based exit, see breakout_52w).
strategy volume_spike {
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
    lb      : Duration = 20d
    lb_min  : Count = 10
    mult    : Scalar = 2.0
    min_ret : Scalar = 0.02
    hold    : Duration = 5d
    qty     : Quantity<Shares> = 100 shares
  }

  rules {
    rel spike(A: Equity, @T)
    spike(A, T) :- volume(A, T, V), mean_prior_window[lb, lb_min, volume](A, T, AvgV), V > AvgV * mult,
        log_price_return(A, T, R), R > min_ret.
  }

  decisions {
    silence hold
    orders  market
    decide(T, trade(A, qty)) :- spike(A, T), flat(A, T).
    decide(T, trade(A, Q)) :- held(A, T, Q0), not bought_within[hold](A, T), Q = -Q0.
  }

  execution equity_default
  report { calendar base   periods 252 per year }
}
