# Volatility-targeted trend on one index tracker (TASK.md): hold weight
# min(1, target vol / realized vol) while the close is above its 200-bar
# moving average, flat otherwise. Daily decisions.
strategy vol_targeted_trend {
  env equities_1d
  resolution @1d
  mode target

  param ma_rows : Count = 200 in 50..300
  param vol_rows : Count = 60 in 20..120
  param target_vol : Scalar = 0.10 in 0.05..0.20
  param ann : Scalar = 252

  # Daily log return from the previous bar.
  rel logret(+A: Equity, @T: Timestamp, -R: Scalar)
  logret(A, T, R) :- close(A, T, P), prev(T, T0), close(A, T0, P0), R = log(P / P0).

  # Simple moving average over the last ma_rows trading bars (rows, not calendar days).
  rel ma(+A: Equity, @T: Timestamp, -M: Price<USD>)
  ma(A, T, M) :- universe(A, T),
      M = mean(P) over (T1 in rows(T, ma_rows, min ma_rows), close(A, T1, P)).

  rel trend_up(-A: Equity, @T: Timestamp)
  trend_up(A, T) :- universe(A, T), close(A, T, P), ma(A, T, M), P > M.

  rel trend_dn(-A: Equity, @T: Timestamp)
  trend_dn(A, T) :- universe(A, T), close(A, T, P), ma(A, T, M), P <= M.

  # Annualized realized volatility: sample std of the last vol_rows daily log returns, times sqrt(ann).
  rel ann_vol(+A: Equity, @T: Timestamp, -S: Scalar)
  ann_vol(A, T, S) :- universe(A, T),
      D = std(R) over (T1 in rows(T, vol_rows, min vol_rows), logret(A, T1, R)),
      S = D * sqrt(ann).

  # The weight that gives the position target_vol, before the cap at one.
  rel vol_weight(+A: Equity, @T: Timestamp, -W: Scalar)
  vol_weight(A, T, W) :- universe(A, T), ann_vol(A, T, S), W = target_vol / S.

  # The cap is written as two disjoint rules: least(1, x) returned x above one in a stress run.
  decide(T, target_weight(A, W)) :- trend_up(A, T), vol_weight(A, T, W), W <= 1.
  decide(T, target_weight(A, 1)) :- trend_up(A, T), vol_weight(A, T, W), W > 1.
  decide(T, target_weight(A, 0)) :- trend_dn(A, T).
}
