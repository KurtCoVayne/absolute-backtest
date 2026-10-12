# Buy when the close sits more than `entry` standard deviations below its
# rolling mean; sell once the z-score recovers above `exit`.
strategy mean_reversion_zscore {
  data {
    class   equity
    need    bars @1d
    need    listing
    require tested
    prefer  latest
  }
  uses  calendar, stat, book
  clock { decide on base }
  universe {
    universe(A, T) :- listed(A, T), traded@base(A, T).
  }

  params {
    n     : Duration = 20d in 10d..60d
    k     : Count = 12
    entry : Scalar = -2.0 in -4.0..-1.0
    exit  : Scalar = 0.0
    qty   : Quantity<Shares> = 100 shares
  }

  rules {
    rel oversold(A: Equity, @T)
    oversold(A, T) :- zscore_window[n, k, close](A, T, Z), Z < entry.
    rel recovered(A: Equity, @T)
    recovered(A, T) :- zscore_window[n, k, close](A, T, Z), Z > exit.
  }

  decisions {
    silence hold
    orders  market
    decide(T, trade(A, qty)) :- oversold(A, T), flat(A, T).
    decide(T, trade(A, Q)) :- recovered(A, T), held(A, T, Q0), Q = -Q0.
  }

  execution equity_default
  report { calendar base   periods 252 per year }
}
