# Canonical 12-1 momentum (data-bundle doc, section 8, stylized facts): at
# each month start, long the n index members with the highest return over
# the past twelve months skipping the last, short the n lowest, four tenths
# of equity a leg. On the five-name synthetic market n is 1; a study on the
# point-in-time universe sets n to a decile (`--param n=50`).
strategy momentum_12_1 {
  env equities_1d_v2
  uses catalog
  resolution @1d
  mode target

  param lookback : Duration = 12mo in 6mo..2y
  param skip : Duration = 1mo in 0d..3mo
  param min_obs : Count = 150
  param min_adv : Notional<USD> = 1_000_000 USD
  param index : Label = "SPX"
  param n : Count = 1 in 1..100
  # Less than half of equity a leg: between rebalances the legs drift apart,
  # and the default policy halts a book whose gross exposure passes equity.
  param gross : Scalar = 0.4

  rel mom(-A: Equity, @T: Timestamp, -M: Scalar)
  mom(A, T, M) :- universe(A, T), member(A, T, index), liquid(A, T, min_adv),
      X = sum(R) over (T1 in window(T, lookback, min min_obs), logret(A, T1, R)),
      Y = sum(R) over (T1 in window(T, skip, min 1), logret(A, T1, R)),
      M = X - Y.

  rel long_leg(-A: Equity, @T: Timestamp)
  long_leg(A, T) :- bar(T), month_start(T), top(n, mom(A, T, M), by (M desc, A asc)).

  rel short_leg(-A: Equity, @T: Timestamp)
  short_leg(A, T) :- bar(T), month_start(T), top(n, mom(A, T, M), by (M asc, A asc)).

  rel n_long(@T: Timestamp, -N: Count)
  n_long(T, N) :- bar(T), N = count(A) over (long_leg(A, T)), N > 0.

  rel n_short(@T: Timestamp, -N: Count)
  n_short(T, N) :- bar(T), N = count(A) over (short_leg(A, T)), N > 0.

  rel in_book(-A: Equity, @T: Timestamp)
  in_book(A, T) :- position(A, T, Q), Q > 0 shares.
  in_book(A, T) :- position(A, T, Q), Q < 0 shares.

  decide(T, target_weight(A, W)) :- long_leg(A, T), n_long(T, N), W = gross / N.
  decide(T, target_weight(A, W)) :- short_leg(A, T), n_short(T, N), W = -gross / N.
  decide(T, target_weight(A, 0)) :- bar(T), month_start(T), in_book(A, T), not long_leg(A, T), not short_leg(A, T).
}
