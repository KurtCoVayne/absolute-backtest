# Buy a close above the prior-year high, with a cooldown against re-entry;
# a time-based exit through the kernel's own decision history: sell once no
# buy was decided within the last `hold` (a state test, retried every bar;
# `lag(T, hold, T0), decided(T0, trade(A, _))` would be a point test that
# misses entries whose T0 + hold is not a bar).
strategy breakout_52w {
  data {
    class   equity
    need    bars @1d
    need    actions, listing
    require tested
    prefer  latest
  }
  uses  calendar, stat, book
  clock { decide on base }
  universe {
    # A name's rows are the days it was listed and traded.
    universe(A, T) :- listed(A, T), traded@base(A, T).
  }

  params {
    lookback     : Duration = 1y in 6mo..2y
    lookback_min : Count = 200
    hold         : Duration = 20d in 5d..60d
    cooldown     : Duration = 60d
    qty          : Quantity<Shares> = 100 shares
  }

  rules {
    rel breakout(A: Equity, @T)
    breakout(A, T) :- close(A, T, P), max_prior_window[lookback, lookback_min, close](A, T, H), P > H.
  }

  decisions {
    silence hold
    orders  market
    decide(T, trade(A, qty)) :- breakout(A, T), flat(A, T), not bought_within[cooldown](A, T).
    decide(T, trade(A, Q)) :- held(A, T, Q0), not bought_within[hold](A, T), Q = -Q0.
  }

  execution equity_default
  report { calendar base   periods 252 per year }
}
