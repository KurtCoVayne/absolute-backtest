strategy mw14 {
  env equities_1w
  resolution @1d
  mode target

  # Cumulative split factor (to compute split-adjusted close)
  rel cum_split(+A: Equity, @T: Timestamp, -C: Scalar)
  cum_split(A, T, 1) :- universe(A, T), not prev(T, _), not split(A, T, _).
  cum_split(A, T, F) :- universe(A, T), not prev(T, _), split(A, T, F).
  cum_split(A, T, C) :- universe(A, T), prev(T, T0), universe(A, T0),
      cum_split(A, T0, C0), split(A, T, F), C = C0 * F.
  cum_split(A, T, C0) :- universe(A, T), prev(T, T0), universe(A, T0),
      cum_split(A, T0, C0), not split(A, T, _).

  # Split-adjusted close
  rel close_adj(+A: Equity, @T: Timestamp, -P: Price<USD>)
  close_adj(A, T, P) :- close(A, T, P0), cum_split(A, T, C), P = P0 * C.

  # Universe eligibility: S&P 1500 member, raw close >= $3, 4-week mean dvol >= $2M
  rel eligible(-A: Equity, @T: Timestamp)
  eligible(A, T) :- universe(A, T), member(A, T, "SP1500"), close(A, T, P), P >= 3 USD/share,
      D = mean(V) over (T1 in rows(T, 4, min 4), dvol(A, T1, V)),
      D >= 2_000_000 USD.

  # Clenow momentum: OLS regression of log(close_adj) on row number
  rel score(-A: Equity, @T: Timestamp, -S: Scalar)
  score(A, T, S) :- eligible(A, T),
      B = ols_beta(L, I) over (T1 in rows(T, 18, min 18), close_adj(A, T1, P), L = log(P), I = 1),
      R = corr(L, I) over (T1 in rows(T, 18, min 18), close_adj(A, T1, P), L = log(P), I = 1),
      S = (exp(52 * B) - 1) * R * R.

  # Rank eligible stocks by score (best first), ties broken by asset id
  rel ranked(-A: Equity, @T: Timestamp, -Rank: Scalar)
  ranked(A, T, Rank) :- score(A, T, _),
      rank(score(A, T, S), by (S desc, A asc), as Rank).

  # SPXTR gate: SPXTR > 52-week mean
  rel gate(-T: Timestamp)
  gate(T) :- universe(_, T), series(T, "SPXTR", V),
      M = mean(SV) over (T1 in window(T, 52 w, min 52), series(T1, "SPXTR", SV)),
      V > M.

  # Liquidity: rank eligible by 4-week mean dvol ascending, top 6 deciles
  rel dvol_4w(+A: Equity, @T: Timestamp, -D: Notional<USD>)
  dvol_4w(A, T, D) :- eligible(A, T),
      D = mean(V) over (T1 in rows(T, 4, min 4), dvol(A, T1, V)).

  rel liq_ranked(-A: Equity, @T: Timestamp, -LiqRank: Scalar)
  liq_ranked(A, T, LiqRank) :- dvol_4w(A, T, _),
      rank(dvol_4w(A, T, D), by (D asc, A asc), as LiqRank).

  rel n_eligible(-T: Timestamp, -N: Scalar)
  n_eligible(T, N) :- eligible(_, T),
      N = 1 / (count(A) over (A in eligible_count(A, T))).

  rel eligible_count(-A: Equity, @T: Timestamp)
  eligible_count(A, T) :- eligible(A, T).

  rel liquid(-A: Equity, @T: Timestamp)
  liquid(A, T) :- liq_ranked(A, T, LR), eligible(_, T),
      N = count(A2) over (A2 in eligible_count(A2, T)),
      LR / N * 10 > 4.

  # Entry: rank <= 7, gate, liquid
  rel entered(-A: Equity, @T: Timestamp)
  entered(A, T) :- ranked(A, T, R), R <= 7, gate(T), liquid(A, T).

  # 40-row mean close (for stay condition)
  rel close_ma40(+A: Equity, @T: Timestamp, -M: Price<USD>)
  close_ma40(A, T, M) :- eligible(A, T),
      M = mean(P) over (T1 in rows(T, 40, min 40), close_adj(A, T1, P)).

  # Stay: rank <= 28 OR close > 40-row mean
  rel may_stay(-A: Equity, @T: Timestamp)
  may_stay(A, T) :- ranked(A, T, R), R <= 28.
  may_stay(A, T) :- close_adj(A, T, P), close_ma40(A, T, M), P > M.

  # Latch: may stay AND (entered this week OR was latched last week)
  rel latched(-A: Equity, @T: Timestamp)
  latched(A, T) :- may_stay(A, T), entered(A, T).
  latched(A, T) :- may_stay(A, T), prev(T, T0), universe(A, T0), latched(A, T0).

  # VIX panic: if gate doesn't hold and VIX > VIX_MA4, scale = 0
  rel panic(-T: Timestamp)
  panic(T) :- universe(_, T), not gate(T),
      series(T, "VIX", VIX), series(T, "VIX_MA4", VIX_MA4), VIX > VIX_MA4.

  # Crowding: if >25% of latched with 40-row mean have close >50% above mean, scale = 0.70; else 1
  rel latched_with_40w(-A: Equity, @T: Timestamp)
  latched_with_40w(A, T) :- latched(A, T), close_ma40(A, T, _).

  rel crowded_check(+A: Equity, @T: Timestamp, -Crowded: Scalar)
  crowded_check(A, T, 1) :- latched_with_40w(A, T), close_adj(A, T, P), close_ma40(A, T, M), P > M * 1.5.
  crowded_check(A, T, 0) :- latched_with_40w(A, T), close_adj(A, T, P), close_ma40(A, T, M), P <= M * 1.5.

  rel scale_val(@T: Timestamp, -Scale: Scalar)
  scale_val(T, 0) :- panic(T).
  scale_val(T, 0.70) :- universe(_, T), not panic(T),
      Crowded = count(A) over (A in crowded_check_filter(A, T)),
      Total = count(A) over (A in latched_with_40w(A, T)),
      Total > 0,
      Crowded / Total > 0.25.
  scale_val(T, 1) :- universe(_, T), not panic(T),
      Crowded = count(A) over (A in crowded_check_filter(A, T)),
      Total = count(A) over (A in latched_with_40w(A, T)),
      Total > 0,
      Crowded / Total <= 0.25.

  rel crowded_check_filter(-A: Equity, @T: Timestamp)
  crowded_check_filter(A, T) :- crowded_check(A, T, 1).

  # Held eligible stocks
  rel held_eligible(-A: Equity, @T: Timestamp)
  held_eligible(A, T) :- latched(A, T), eligible(A, T).

  # Number of held eligible stocks
  rel num_held(-T: Timestamp, -N: Scalar)
  num_held(T, N) :- universe(_, T),
      Count = count(A) over (A in held_eligible(A, T)),
      N = 1 / Count.

  # Target weight for each held stock: min(scale / N, 0.15), only if > 0.000001
  rel target_w(@T: Timestamp, +A: Equity, -W: Scalar)
  target_w(T, A, W) :- held_eligible(A, T), scale_val(T, Scale), num_held(T, NInv),
      W0 = Scale * NInv,
      W1 = least(W0, 0.15),
      W1 > 0.000001,
      W = W1.

  decide(T, target_weight(A, W)) :- target_w(T, A, W).
}
