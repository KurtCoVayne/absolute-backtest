strategy momentum_liquid_monthly {
  env equities_1d_v2
  uses catalog
  resolution @1d
  mode target

  param min_adv : Notional<USD> = 1_000_000 USD
  param top_n : Count = 10
  param lookback : Duration = 12mo
  param skip_recent : Duration = 1mo
  param min_obs : Count = 50

  # Momentum over the lookback period excluding the most recent month
  rel momentum(-A: Equity, @T: Timestamp, -R: Scalar)
  momentum(A, T, R) :- universe(A, T), month_start(T),
      liquid(A, T, min_adv),
      R = mean(R1) over (
          T1 in prior_window(T, lookback, min min_obs),
          lag(T, skip_recent, T_cutoff),
          T1 <= T_cutoff,
          ret(A, T1, R1)
      ).

  # Helper: month start bars
  rel month_bar(-T: Timestamp)
  month_bar(T) :- bar(T), month_start(T).

  # Select top 10 by momentum at month start
  rel selected(-A: Equity, @T: Timestamp)
  selected(A, T) :- month_bar(T), top(top_n, momentum(A, T, R), by (R desc, A asc, T asc)).

  # Equal weight for selected stocks
  rel weight(-A: Equity, @T: Timestamp, -W: Scalar)
  weight(A, T, W) :- selected(A, T), W = 1 / top_n.

  # Rebalance at month start
  decide(T, target_weight(A, W)) :- weight(A, T, W).
  decide(T, target_weight(A, 0)) :- month_bar(T), universe(A, T), not weight(A, T, _).
}
