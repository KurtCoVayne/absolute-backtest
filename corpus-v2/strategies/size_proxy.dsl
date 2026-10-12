# Canonical size, by the proxy the catalog has (data-bundle doc, section 8,
# stylized facts): market capitalisation is not a catalog relation, so
# average dollar volume stands in for it. At each month start, long the n
# index members with the smallest average dollar volume, short the n
# largest, four tenths of the capital a leg.
strategy size_proxy {
  data {
    class   equity
    need    bars @1d
    need    actions, listing
    need    lists "SPX"
    require tested
    prefer  latest
  }
  uses  calendar, catalog, stat, bars, book
  clock { decide on base }
  universe {
    universe(A, T) :- listed(A, T), traded@base(A, T).
  }

  params {
    lookback : Duration = 60d in 20d..1y
    min_obs  : Count = 40
    n        : Count = 1 in 1..100
    # Less than half of the capital a leg: between rebalances the legs drift
    # apart, and the default policy halts a book whose gross exposure passes it.
    gross    : Scalar = 0.4
  }

  rules {
    rel size(A: Equity, @T, D: Notional<USD>)
    size(A, T, D) :- universe(A, T), member(A, T, "SPX"), mean_window[lookback, min_obs, dollar_volume](A, T, D).

    rel long_leg(A: Equity, @T)
    long_leg(A, T) :- bar(T), month_start(T), top[n](size(A, T, D) by (D asc, A asc)) within (T).
    rel short_leg(A: Equity, @T)
    short_leg(A, T) :- bar(T), month_start(T), top[n](size(A, T, D) by (D desc, A asc)) within (T).

    rel n_long(@T, N: Count)
    n_long(T, N) :- bar(T), N = count(A) over (long_leg(A, T)) within (T), N > 0.
    rel n_short(@T, N: Count)
    n_short(T, N) :- bar(T), N = count(A) over (short_leg(A, T)) within (T), N > 0.
  }

  decisions {
    silence hold
    orders  market
    decide(T, hold(A, W)) :- long_leg(A, T), n_long(T, N), W = gross / N.
    decide(T, hold(A, W)) :- short_leg(A, T), n_short(T, N), W = -gross / N.
    decide(T, hold(A, 0)) :- bar(T), month_start(T), in_book(A, T), not long_leg(A, T), not short_leg(A, T).
  }

  execution equity_default
  report { calendar base   periods 252 per year }
}
