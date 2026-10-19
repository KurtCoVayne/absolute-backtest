# Canonical 12-1 momentum (data-bundle doc, section 8, stylized facts): at
# each month start, long the n index members with the highest return over
# the past twelve months skipping the last, short the n lowest, four tenths
# of the capital a leg. On the five-name synthetic market n is 1; a study on
# the point-in-time universe sets n to a decile (`--param n=50`).
strategy momentum_12_1 {
  data {
    class   equity
    need    bars @1d
    need    actions, listing
    need    lists "SPX"
    require reviewed
    prefer  latest
  }
  uses  calendar, catalog, stat, bars, book
  clock { decide on base }
  universe {
    universe(A, T) :- listed(A, T), traded@base(A, T).
  }

  params {
    lookback : Duration = 12mo in 6mo..2y
    skip     : Duration = 1mo in 0d..3mo
    min_obs  : Count = 150
    min_adv  : Notional<USD> = 1_000_000 USD
    n        : Count = 1 in 1..100
    # Less than half of the capital a leg: between rebalances the legs drift
    # apart, and the default policy halts a book whose gross exposure passes it.
    gross    : Scalar = 0.4
  }

  rules {
    rel eligible(A: Equity, @T)
    eligible(A, T) :- universe(A, T), member(A, T, "SPX"), liquid[min_adv](A, T).

    # Twelve months of log total return less the most recent month.
    rel mom(A: Equity, @T, M: Scalar)
    mom(A, T, M) :- eligible(A, T),
        sum_window[lookback, min_obs, log_return](A, T, X),
        sum_window[skip, 1, log_return](A, T, Y),
        M = X - Y.

    rel long_leg(A: Equity, @T)
    long_leg(A, T) :- bar(T), month_start(T), top[n](mom(A, T, M) by (M desc, A asc)) within (T).
    rel short_leg(A: Equity, @T)
    short_leg(A, T) :- bar(T), month_start(T), top[n](mom(A, T, M) by (M asc, A asc)) within (T).

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
