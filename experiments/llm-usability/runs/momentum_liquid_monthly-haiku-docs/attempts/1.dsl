strategy momentum_liquid_monthly {
  env equities_1d_v2
  uses catalog
  resolution @1d
  mode target

  param min_adv : Notional<USD> = 1_000_000 USD
  param top_n : Count = 10

  # Momentum over 12 months excluding the most recent month
  rel momentum(-A: Equity, @T: Timestamp, -R: Scalar)
  momentum(A, T, R) :- universe(A, T), month_start(T),
      liquid(A, T, min_adv),
      R = mean(R1) over (
          T1 in prior_window(T, 12mo, min 50),
          lag(T, 1mo, T_cutoff),
          T1 <= T_cutoff,
          ret(A, T1, R1)
      ).

  # Select top 10 by momentum
  rel selected(-A: Equity, @T: Timestamp)
  selected(A, T) :- top(top_n, momentum(A, T, R), by (R desc)).

  # Equal weight for selected stocks
  rel weight(-A: Equity, @T: Timestamp, -W: Scalar)
  weight(A, T, W) :- selected(A, T), W = 1 / top_n.

  # Rebalance at month start
  decide(T, target_weight(A, W)) :- weight(A, T, W).
  decide(T, target_weight(A, 0)) :- month_start(T), universe(A, T), not weight(A, T, _).
}
