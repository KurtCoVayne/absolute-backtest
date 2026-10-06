strategy mw14 {
  env equities_1w
  resolution @1d
  mode target

  # Bar tracking for temporal operations
  rel bar(@T: Timestamp)
  bar(T) :- universe(_, T).

  # Helper: check if T has a previous bar
  rel has_prev(@T: Timestamp)
  has_prev(T) :- bar(T), prev(T, _).

  # Cumulative split factor (to compute split-adjusted close)
  rel cum_split(+A: Equity, @T: Timestamp, -C: Scalar)
  cum_split(A, T, 1) :- universe(A, T), not has_prev(T), not split(A, T, _).
  cum_split(A, T, F) :- universe(A, T), not has_prev(T), split(A, T, F).
  cum_split(A, T, C) :- universe(A, T), has_prev(T),
      prev(T, T0), universe(A, T0), cum_split(A, T0, C0), split(A, T, F), C = C0 * F.
  cum_split(A, T, C0) :- universe(A, T), has_prev(T),
      prev(T, T0), universe(A, T0), cum_split(A, T0, C0), not split(A, T, _).

  # Split-adjusted close
  rel close_adj(+A: Equity, @T: Timestamp, -P: Price<USD>)
  close_adj(A, T, P) :- close(A, T, P0), cum_split(A, T, C), P = P0 * C.

  # Universe eligibility: S&P 1500 member, raw close >= $3, 4-week mean dvol >= $2M
  rel eligible(+A: Equity, @T: Timestamp)
  eligible(A, T) :- universe(A, T), member(A, T, "SP1500"), close(A, T, P), P >= 3 USD/share,
      D = mean(V) over (T1 in rows(T, 4, min 4), dvol(A, T1, V)),
      D >= 2_000_000 USD.

  # Clenow momentum: OLS regression of log(close_adj) on row number
  rel score(+A: Equity, @T: Timestamp, -S: Scalar)
  score(A, T, S) :- eligible(A, T),
      B = ols_beta(L, I) over (T1 in rows(T, 18, min 18), close_adj(A, T1, P), L = log(P / (1 USD/share)), I = 1),
      R = corr(L, I) over (T1 in rows(T, 18, min 18), close_adj(A, T1, P), L = log(P / (1 USD/share)), I = 1),
      S = (exp(52 * B) - 1) * R * R.

  # Rank eligible stocks by score (best first), ties broken by asset id
  rel ranked(+A: Equity, @T: Timestamp, -Rank: Scalar)
  ranked(A, T, Rank) :- eligible(A, T),
      rank(score(A, T, S), by (S desc, A asc), as Rank).

  # SPXTR gate: SPXTR > 52-week mean
  rel gate(@T: Timestamp)
  gate(T) :- universe(_, T), series(T, "SPXTR", V),
      M = mean(SV) over (T1 in window(T, 52w, min 52), series(T1, "SPXTR", SV)),
      V > M.

  # 4-week mean dollar volume
  rel dvol_4w(+A: Equity, @T: Timestamp, -D: Notional<USD>)
  dvol_4w(A, T, D) :- eligible(A, T),
      D = mean(V) over (T1 in rows(T, 4, min 4), dvol(A, T1, V)).

  # Rank eligible by 4-week mean dvol (ascending)
  rel liq_rank(+A: Equity, @T: Timestamp, -Rank: Scalar)
  liq_rank(A, T, Rank) :- eligible(A, T),
      rank(dvol_4w(A, T, D), by (D asc, A asc), as Rank).

  # Count eligible stocks at T
  rel count_eligible(@T: Timestamp, -N: Scalar)
  count_eligible(T, N) :- bar(T),
      C = count(A) over (T1 in window(T, 0d, min 1), eligible(A, T1)),
      N = C.

  # Liquidity: top 6 deciles (rank / N * 10 > 4)
  rel liquid(+A: Equity, @T: Timestamp)
  liquid(A, T) :- liq_rank(A, T, LR), count_eligible(T, N), LR / N * 10 > 4.

  # Entry: rank <= 7, gate, liquid
  rel entered(+A: Equity, @T: Timestamp)
  entered(A, T) :- ranked(A, T, R), R <= 7, gate(T), liquid(A, T).

  # 40-row mean close (for stay condition)
  rel close_ma40(+A: Equity, @T: Timestamp, -M: Price<USD>)
  close_ma40(A, T, M) :- eligible(A, T),
      M = mean(P) over (T1 in rows(T, 40, min 40), close_adj(A, T1, P)).

  # Stay: rank <= 28 OR close > 40-row mean
  rel may_stay(+A: Equity, @T: Timestamp)
  may_stay(A, T) :- ranked(A, T, R), R <= 28.
  may_stay(A, T) :- close_adj(A, T, P), close_ma40(A, T, M), P > M.

  # Latch: may stay AND (entered this week OR was latched last week)
  rel latched(+A: Equity, @T: Timestamp)
  latched(A, T) :- may_stay(A, T), entered(A, T).
  latched(A, T) :- may_stay(A, T), prev(T, T0), universe(A, T0), latched(A, T0).

  # VIX panic: if gate doesn't hold and VIX > VIX_MA4, scale = 0
  rel panic(@T: Timestamp)
  panic(T) :- universe(_, T), not gate(T),
      series(T, "VIX", VIX), series(T, "VIX_MA4", VIX_MA4), VIX > VIX_MA4.

  # Crowding: if >25% of latched with 40-row mean have close >50% above mean, scale = 0.70; else 1
  rel latched_with_40w(+A: Equity, @T: Timestamp)
  latched_with_40w(A, T) :- latched(A, T), close_ma40(A, T, _).

  rel crowded_stock(+A: Equity, @T: Timestamp)
  crowded_stock(A, T) :- latched_with_40w(A, T), close_adj(A, T, P), close_ma40(A, T, M), P > M * 1.5.

  # Count for crowding check
  rel count_crowded(@T: Timestamp, -Crowded: Scalar, -Total: Scalar)
  count_crowded(T, Crowded, Total) :- bar(T),
      Crowded = count(A) over (T1 in window(T, 0d, min 1), crowded_stock(A, T1)),
      Total = count(A) over (T1 in window(T, 0d, min 1), latched_with_40w(A, T1)).

  rel scale_val(@T: Timestamp, -Scale: Scalar)
  scale_val(T, 0) :- panic(T).
  scale_val(T, 0.70) :- bar(T), not panic(T), count_crowded(T, C, Tot), Tot > 0, C / Tot > 0.25.
  scale_val(T, 1) :- bar(T), not panic(T), count_crowded(T, C, Tot), Tot > 0, C / Tot <= 0.25.
  scale_val(T, 1) :- bar(T), not panic(T), count_crowded(T, _, Tot), Tot = 0.

  # Held eligible stocks
  rel held_eligible(+A: Equity, @T: Timestamp)
  held_eligible(A, T) :- latched(A, T), eligible(A, T).

  # Count held eligible
  rel count_held(@T: Timestamp, -N: Scalar)
  count_held(T, N) :- bar(T),
      C = count(A) over (T1 in window(T, 0d, min 1), held_eligible(A, T1)),
      N = C.

  # Target weight for each held stock: min(scale / N, 0.15), only if > 0.000001
  decide(T, target_weight(A, W)) :-
      held_eligible(A, T), scale_val(T, Scale), count_held(T, N), N > 0,
      W0 = Scale / N,
      W1 = least(W0, 0.15),
      W1 > 0.000001,
      W = W1.
}
