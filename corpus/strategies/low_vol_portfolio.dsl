# Monthly: hold the n lowest-volatility names, weighted by inverse volatility.
strategy low_vol_portfolio {
  env equities_1d
  uses features
  resolution @1d
  mode target

  param lb : Duration = 60d in 20d..250d
  param lb_min : Count = 40
  param n : Count = 3

  rel vol(-A: Equity, @T: Timestamp, -V: Scalar)
  vol(A, T, V) :- universe(A, T), realized_vol(A, T, lb, lb_min, V), V > 0.

  rel chosen(-A: Equity, @T: Timestamp, -V: Scalar)
  chosen(A, T, V) :- bar(T), month_start(T), top(n, vol(A, T, V), by (V asc, A asc)).

  rel inv_sum(@T: Timestamp, -S: Scalar)
  inv_sum(T, S) :- bar(T), S = sum(1 / V) over (chosen(_, T, V)).

  decide(T, target_weight(A, W)) :- chosen(A, T, V), inv_sum(T, S), W = (1 / V) / S.
  decide(T, target_weight(A, 0)) :- bar(T), month_start(T), held(A, T, _), not chosen(A, T, _).
}
