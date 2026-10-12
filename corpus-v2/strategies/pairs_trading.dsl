# Classic pair: hedge ratio by OLS over log returns, z-score of the residual
# spread. Bivariate aggregates align the two series on T1 by construction
# (section 4, bivariate aggregates). A delta is signed, so one rule closes a
# leg whichever side it is on.
# allow: W6  (the ticker literals are a snapshot of the bundle date)
strategy pairs_trading {
  data {
    class   equity
    need    bars @1d
    need    listing
    require tested
    prefer  latest
  }
  uses  calendar, bars, book
  clock { decide on base }
  universe {
    universe(A, T) :- listed(A, T), traded@base(A, T).
  }

  params {
    y      : Equity = "AAA"
    x      : Equity = "BBB"
    lb     : Duration = 60d in 30d..250d
    lb_min : Count = 40
    entry  : Scalar = 2.0
    exit   : Scalar = 0.5
    qty    : Quantity<Shares> = 100 shares
  }

  rules {
    rel beta(@T, B: Scalar)
    beta(T, B) :- bar(T),
        B = ols_slope(RY, RX) over (T1 in window[lb] min lb_min, log_price_return(y, T1, RY), log_price_return(x, T1, RX)).

    rel spread(@T, S: Scalar)
    spread(T, S) :- beta(T, B), log_price_return(y, T, RY), log_price_return(x, T, RX), S = RY - B * RX.

    rel spread_z(@T, Z: Scalar)
    spread_z(T, Z) :- spread(T, S),
        M = mean(S1) over (T1 in window[lb] min lb_min, spread(T1, S1)),
        D = std(S1) over (T1 in window[lb] min lb_min, spread(T1, S1)),
        Z = (S - M) / D.

    # The pair is opened and closed as one.
    rel pair_flat(@T)
    pair_flat(T) :- flat(y, T), flat(x, T).
  }

  decisions {
    silence hold
    orders  market
    # Short the spread: short y, long x.
    decide(T, trade(y, Q)) :- spread_z(T, Z), Z > entry, pair_flat(T), Q = -qty.
    decide(T, trade(x, qty)) :- spread_z(T, Z), Z > entry, pair_flat(T).
    # Long the spread: long y, short x.
    decide(T, trade(y, qty)) :- spread_z(T, Z), Z < -entry, pair_flat(T).
    decide(T, trade(x, Q)) :- spread_z(T, Z), Z < -entry, pair_flat(T), Q = -qty.
    # Close both legs once the spread has reverted.
    decide(T, trade(y, Q)) :- spread_z(T, Z), abs(Z) < exit, position(y, T, Q0), Q = -Q0.
    decide(T, trade(x, Q)) :- spread_z(T, Z), abs(Z) < exit, position(x, T, Q0), Q = -Q0.
  }

  execution equity_default
  report { calendar base   periods 252 per year }
}
