# Daily momentum top-n decided on the daily calendar of a minute bundle: the
# positive cross-calendar case. The daily bars are the views of section 3.3,
# derived by the kernel from the minute bars; the program names no
# resampling.
strategy resampled_momentum {
  data {
    class   equity
    need    bars @1m
    need    listing
    require tested
    prefer  latest
  }
  uses  calendar, book
  calendars { daily = days }
  clock { decide on daily }
  universe {
    # A name's rows are the days it traded.
    universe(A, T) :- listed(A, T), traded@daily(A, T).
  }

  params {
    lookback : Duration = 20d in 10d..120d
    n        : Count = 2
  }

  rules {
    rel mom(A: Equity, @T, M: Scalar)
    mom(A, T, M) :- universe(A, T), close@daily(A, T, P1), lag(T, lookback, T0), close@daily(A, T0, P0), M = P1 / P0 - 1.

    rel chosen(A: Equity, @T)
    chosen(A, T) :- bar(T), top[n](mom(A, T, M) by (M desc, A asc)) within (T).

    rel n_chosen(@T, N: Count)
    n_chosen(T, N) :- bar(T), N = count(A) over (chosen(A, T)) within (T), N > 0.
  }

  decisions {
    silence hold
    orders  market
    decide(T, hold(A, W)) :- chosen(A, T), n_chosen(T, N), W = 1 / N.
    decide(T, hold(A, 0)) :- position(A, T, _), not chosen(A, T).
  }

  execution equity_default
  report { calendar daily   periods 252 per year }
}
