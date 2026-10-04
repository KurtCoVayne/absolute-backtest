# Long the top momentum quantile, short the bottom one. Names that drift into
# the middle band are flattened positively: `not long_leg` would be rejected
# by WF-5, since the legs depend on `close`.
strategy long_short_quantile {
  env equities_1d
  uses features
  resolution @1d
  mode target

  param lookback : Duration = 6mo
  param skip : Duration = 1mo
  param q : Scalar = 0.8
  param gross : Scalar = 1.0

  rel mom(-A: Equity, @T: Timestamp, -M: Scalar)
  mom(A, T, M) :- universe(A, T), momentum(A, T, lookback, skip, M).

  rel cutoffs(@T: Timestamp, -Hi: Scalar, -Lo: Scalar)
  cutoffs(T, Hi, Lo) :- bar(T),
      Hi = quantile(M, q) over (mom(_, T, M)),
      Lo = quantile(M, 1 - q) over (mom(_, T, M)).

  rel long_leg(-A: Equity, @T: Timestamp)
  long_leg(A, T) :- cutoffs(T, Hi, _), mom(A, T, M), M >= Hi.

  rel short_leg(-A: Equity, @T: Timestamp)
  short_leg(A, T) :- cutoffs(T, _, Lo), mom(A, T, M), M <= Lo.

  rel n_long(@T: Timestamp, -N: Count)
  n_long(T, N) :- bar(T), N = count(A) over (long_leg(A, T)), N > 0.

  rel n_short(@T: Timestamp, -N: Count)
  n_short(T, N) :- bar(T), N = count(A) over (short_leg(A, T)), N > 0.

  decide(T, target_weight(A, W)) :- long_leg(A, T), n_long(T, N), W = gross / N.
  decide(T, target_weight(A, W)) :- short_leg(A, T), n_short(T, N), W = -gross / N.
  decide(T, target_weight(A, 0)) :- position(A, T, _), mom(A, T, M), cutoffs(T, Hi, Lo), M < Hi, M > Lo.
}
