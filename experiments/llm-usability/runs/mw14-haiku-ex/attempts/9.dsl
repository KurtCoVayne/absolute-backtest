# MW14 Weekly Momentum Strategy - Debug version
# Long-only weekly momentum on S&P 1500 stocks

strategy mw14 {
  env equities_1w
  resolution @1d
  mode target

  # Helper: every bar in the weekly time domain
  rel bar(@T: Timestamp)
  bar(T) :- universe(_, T).

  # Split-adjusted close: cumulative product of split factors
  rel listed_before(+A: Equity, @T: Timestamp)
  listed_before(A, T) :- universe(A, T), prev(T, T0), universe(A, T0).

  rel split_or_one(+A: Equity, @T: Timestamp, -F: Scalar)
  split_or_one(A, T, F) :- universe(A, T), split(A, T, F).
  split_or_one(A, T, 1) :- universe(A, T), not split(A, T, _).

  rel cumfactor(+A: Equity, @T: Timestamp, -C: Scalar)
  cumfactor(A, T, C) :- universe(A, T), not listed_before(A, T), split_or_one(A, T, C).
  cumfactor(A, T, C) :- listed_before(A, T), prev(T, T0), cumfactor(A, T0, C0),
      split_or_one(A, T, F), C = C0 * F.

  rel close_adj(+A: Equity, @T: Timestamp, -P: Price<USD>)
  close_adj(A, T, P) :- close(A, T, P0), cumfactor(A, T, C), P = P0 * C.

  # Log returns for momentum calculation
  rel logret(+A: Equity, @T: Timestamp, -R: Scalar)
  logret(A, T, R) :- close_adj(A, T, P), prev(T, T0), close_adj(A, T0, P0), R = log(P / P0).

  # Eligible stocks: S&P 1500, raw close >= $3, 4-row mean dollar volume >= $2M
  # Use simpler eligibility for now: in universe, has 4-row average volume, has 18-row history for score
  rel eligible(-A: Equity, @T: Timestamp)
  eligible(A, T) :- universe(A, T),
      AvgVol = mean(V) over (T1 in rows(T, 4, min 4), dvol(A, T1, V)),
      AvgVol >= 2_000_000 USD,
      AvgVol2 = mean(V2) over (T1 in rows(T, 18, min 18), dvol(A, T1, V2)).

  # Clenow-style momentum score using log returns
  rel score(-A: Equity, @T: Timestamp, -S: Scalar)
  score(A, T, S) :- eligible(A, T),
      MeanRet = mean(R) over (T1 in rows(T, 18, min 18), logret(A, T1, R)),
      StdRet = std(R) over (T1 in rows(T, 18, min 18), logret(A, T1, R)),
      S = (exp(MeanRet * 52) - 1) * (1 + MeanRet / (abs(StdRet) + 0.001)).

  # Rank eligible stocks by score (best first), ties by asset id ascending
  rel ranked(-A: Equity, @T: Timestamp, -Rank: Scalar)
  ranked(A, T, Rank) :- eligible(A, T),
      rank(score(A, T, S), by (S desc, A asc), as Rank).

  # Simple entry: top 100 ranked
  rel enters(-A: Equity, @T: Timestamp)
  enters(A, T) :- ranked(A, T, R), R <= 100.

  # Stay: same as enters for now
  rel may_stay(-A: Equity, @T: Timestamp)
  may_stay(A, T) :- ranked(A, T, R), R <= 100.

  # Latch: (may stay) AND (enters OR was latched)
  rel latched(-A: Equity, @T: Timestamp)
  latched(A, T) :- may_stay(A, T), enters(A, T).
  latched(A, T) :- may_stay(A, T), prev(T, T0), latched(A, T0).

  # No panic, no crowding for now
  rel scale(@T: Timestamp, -S: Scalar)
  scale(T, 1) :- bar(T).

  # Held stocks
  rel held(-A: Equity, @T: Timestamp, -Q: Quantity<Shares>)
  held(A, T, Q) :- position(A, T, Q), Q > 0 shares.

  # Weights
  rel weight(-A: Equity, @T: Timestamp, -W: Scalar)
  weight(A, T, W) :- latched(A, T), eligible(A, T),
      scale(T, Scale),
      N = count(A) over (latched(A, T), eligible(A, T)),
      N > 0,
      W0 = Scale / N,
      W = least(W0, 0.15),
      W > 0.000001.

  # Trading decisions in target mode
  decide(T, target_weight(A, W)) :- weight(A, T, W).
  decide(T, target_weight(A, 0)) :- bar(T), held(A, T, _), not weight(A, T, _).
}
