# MW14 Weekly Momentum Strategy
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
  rel eligible(-A: Equity, @T: Timestamp)
  eligible(A, T) :- universe(A, T), member(A, T, "SP1500"),
      close(A, T, C), C >= 3 USD/share,
      AvgVol = mean(V) over (T1 in rows(T, 4, min 4), dvol(A, T1, V)),
      AvgVol >= 2_000_000 USD.

  # Clenow-style momentum score using log returns
  # Approximation: mean return (annualized) * correlation of return with position
  rel score(-A: Equity, @T: Timestamp, -S: Scalar)
  score(A, T, S) :- eligible(A, T),
      MeanRet = mean(R) over (T1 in rows(T, 18, min 18), logret(A, T1, R)),
      StdRet = std(R) over (T1 in rows(T, 18, min 18), logret(A, T1, R)),
      S = (exp(MeanRet * 52) - 1) * (1 + MeanRet / (abs(StdRet) + 0.001)).

  # Rank eligible stocks by score (best first), ties by asset id ascending
  rel ranked(-A: Equity, @T: Timestamp, -Rank: Scalar)
  ranked(A, T, Rank) :- eligible(A, T),
      rank(score(A, T, S), by (S desc, A asc), as Rank).

  # Gate: SPXTR above its 52-week mean
  rel spxtr_val(@T: Timestamp, -V: Scalar)
  spxtr_val(T, V) :- series(T, "SPXTR", V).

  rel gate_holds(@T: Timestamp)
  gate_holds(T) :- spxtr_val(T, V),
      Mean = mean(V1) over (T1 in window(T, 52w, min 52), spxtr_val(T1, V1)),
      V > Mean.

  # Liquidity decile: top 6 deciles by dollar volume
  rel liquid_rank(-A: Equity, @T: Timestamp, -LiqRank: Scalar)
  liquid_rank(A, T, LiqRank) :- eligible(A, T),
      rank(dvol(A, T, D), by (D asc, A asc), as LiqRank).

  rel n_eligible(@T: Timestamp, -N: Count)
  n_eligible(T, N) :- bar(T),
      N = count(A) over (eligible(A, T)).

  rel liquid(-A: Equity, @T: Timestamp)
  liquid(A, T) :- liquid_rank(A, T, LR), n_eligible(T, N),
      LR / (N / 10) > 4.

  # Entry: rank <= 7, gate holds, liquid
  rel enters(-A: Equity, @T: Timestamp)
  enters(A, T) :- ranked(A, T, R), R <= 7, gate_holds(T), liquid(A, T).

  # Stay: rank <= 28 OR close > 40-row mean
  rel may_stay(-A: Equity, @T: Timestamp)
  may_stay(A, T) :- ranked(A, T, R), R <= 28.
  may_stay(A, T) :- eligible(A, T), close_adj(A, T, P),
      Mean = mean(P1) over (T1 in rows(T, 40, min 40), close_adj(A, T1, P1)),
      P > Mean.

  # Latch: (may stay) AND (enters OR was latched)
  rel latched(-A: Equity, @T: Timestamp)
  latched(A, T) :- may_stay(A, T), enters(A, T).
  latched(A, T) :- may_stay(A, T), prev(T, T0), latched(A, T0).

  # VIX check for panic
  rel vix_val(@T: Timestamp, -V: Scalar)
  vix_val(T, V) :- series(T, "VIX", V).

  rel vix_ma4_val(@T: Timestamp, -V: Scalar)
  vix_ma4_val(T, V) :- series(T, "VIX_MA4", V).

  # Panic: if neither gate holds nor VIX <= VIX_MA4, scale = 0
  rel panic(@T: Timestamp)
  panic(T) :- bar(T), not gate_holds(T),
      C = count(T) over (vix_val(T, V), vix_ma4_val(T, M), V > M),
      C > 0.

  # Crowding helper: identify crowded stocks
  rel crowded(-A: Equity, @T: Timestamp)
  crowded(A, T) :- latched(A, T), eligible(A, T),
      close_adj(A, T, P),
      Mean = mean(P1) over (T1 in rows(T, 40, min 40), close_adj(A, T1, P1)),
      P > Mean * 1.5.

  # Crowding counts
  rel crowding_check(@T: Timestamp, -Crowded: Count, -Total: Count)
  crowding_check(T, Crowded, Total) :- bar(T),
      Crowded = count(A) over (crowded(A, T)),
      Total = count(A) over (latched(A, T), eligible(A, T)).

  # Crowding: if > 25% of latched eligible have close > 50% above 40-row mean, scale = 0.70
  rel is_crowded(@T: Timestamp)
  is_crowded(T) :- crowding_check(T, Crowded, Total),
      Total > 0, Crowded / Total > 0.25.

  rel crowding_scale(@T: Timestamp, -Scale: Scalar)
  crowding_scale(T, 0.70) :- is_crowded(T).
  crowding_scale(T, 1) :- bar(T), not is_crowded(T).

  # Scale: 0 if panic, else from crowding
  rel scale(@T: Timestamp, -S: Scalar)
  scale(T, 0) :- panic(T).
  scale(T, S) :- bar(T), not panic(T), crowding_scale(T, S).

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
