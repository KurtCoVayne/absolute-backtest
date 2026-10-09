# Attempt 1: long when close > 200-bar average, flat otherwise,
# weight = min(1, target vol / trailing annualized realized vol).
strategy vol_targeted_trend {
  env equities_1d
  uses features
  resolution @1d
  mode target

  param ma_bars : Count = 200 in 50..300
  param vol_bars : Count = 20 in 10..60
  param target_vol : Scalar = 0.10 in 0.05..0.20
  param trading_days : Scalar = 252

  rel ma(+A: Equity, @T: Timestamp, -M: Price<USD>)
  ma(A, T, M) :- universe(A, T),
      M = mean(P) over (T1 in rows(T, ma_bars, min ma_bars), close(A, T1, P)).

  rel ann_vol(+A: Equity, @T: Timestamp, -V: Scalar)
  ann_vol(A, T, V) :- universe(A, T),
      S = std(R) over (T1 in rows(T, vol_bars, min vol_bars), logret(A, T1, R)),
      V = S * sqrt(trading_days).

  rel above_ma(+A: Equity, @T: Timestamp)
  above_ma(A, T) :- close(A, T, P), ma(A, T, M), P > M.

  rel at_or_below_ma(+A: Equity, @T: Timestamp)
  at_or_below_ma(A, T) :- close(A, T, P), ma(A, T, M), P <= M.

  decide(T, target_weight(A, W)) :- above_ma(A, T), ann_vol(A, T, V),
      W = least(1, target_vol / V).
  decide(T, target_weight(A, 0)) :- at_or_below_ma(A, T).
}
