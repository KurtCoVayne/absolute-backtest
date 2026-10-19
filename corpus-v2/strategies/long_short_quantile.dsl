# Long the top momentum quantile, short the bottom one. Names that drift into
# the middle band are flattened positively: `not long_leg` would be rejected
# by WF-5, since the legs read `close`, which this demand does not require
# complete.
strategy long_short_quantile {
  data {
    class   equity
    need    bars @1d
    need    actions, listing
    require reviewed
    prefer  latest
  }
  uses  calendar, book
  clock { decide on base }
  universe {
    universe(A, T) :- listed(A, T), traded@base(A, T).
  }

  params {
    lookback : Duration = 6mo
    skip     : Duration = 1mo
    q        : Scalar = 0.8
    # Half of the capital per leg: the book's gross exposure equals its
    # capital, which is the most the default execution allows (leverage halts the run).
    gross    : Scalar = 0.5
  }

  rules {
    rel mom(A: Equity, @T, M: Scalar)
    mom(A, T, M) :- universe(A, T), lag(T, skip, T1), close(A, T1, P1),
        lag(T, lookback, T0), close(A, T0, P0), M = P1 / P0 - 1.

    rel cutoffs(@T, Hi: Scalar, Lo: Scalar)
    cutoffs(T, Hi, Lo) :- bar(T),
        Hi = quantile(M, q) over (mom(_, T, M)) within (T),
        Lo = quantile(M, 1 - q) over (mom(_, T, M)) within (T).

    rel long_leg(A: Equity, @T)
    long_leg(A, T) :- cutoffs(T, Hi, _), mom(A, T, M), M >= Hi.
    rel short_leg(A: Equity, @T)
    short_leg(A, T) :- cutoffs(T, _, Lo), mom(A, T, M), M <= Lo.

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
    decide(T, hold(A, 0)) :- position(A, T, _), mom(A, T, M), cutoffs(T, Hi, Lo), M < Hi, M > Lo.
  }

  execution equity_default
  report { calendar base   periods 252 per year }
}
