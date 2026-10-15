# One asset, sized so that realized volatility per bar of the decision
# calendar hits a target, capped at full weight.
# allow: W6  (the ticker literal is a snapshot of the bundle date)
strategy volatility_targeting {
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
    asset      : Equity = "SPY"
    target_vol : Scalar = 0.01 in 0.002..0.05
    lb         : Duration = 20d
    lb_min     : Count = 12
    max_weight : Scalar = 1.0
  }

  rules {
    rel weight(@T, W: Scalar)
    weight(T, W) :- bar(T), std_window[lb, lb_min, log_price_return](asset, T, V), V > 0,
        W = least(max_weight, target_vol / V).
  }

  decisions {
    silence hold
    orders  market
    decide(T, hold(asset, W)) :- weight(T, W).
  }

  execution equity_default
  report { calendar base   periods 252 per year }
}
