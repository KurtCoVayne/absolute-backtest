# One asset, sized so that realized per-bar volatility hits a target, capped
# at full weight. v1 does not scale volatility across resolutions, so the
# target is quoted per bar.
# allow: W6  (the ticker literal is a snapshot of the bundle date)
strategy volatility_targeting {
  env equities_1d
  uses features
  resolution @1d
  mode target

  param asset : Equity = "SPY"
  param target_vol : Scalar = 0.01 in 0.002..0.05
  param lb : Duration = 20d
  param lb_min : Count = 12
  param max_weight : Scalar = 1.0

  rel weight(@T: Timestamp, -W: Scalar)
  weight(T, W) :- bar(T), realized_vol(asset, T, lb, lb_min, V), V > 0, W = least(max_weight, target_vol / V).

  decide(T, target_weight(asset, W)) :- weight(T, W).
}
