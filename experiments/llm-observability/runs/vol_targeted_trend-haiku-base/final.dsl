# Attempt 5 (final candidate). Long, sized to a volatility target, while the
# close is above its 200-bar average; flat otherwise. Daily decisions.
#   weight = min(1, target_vol / annualized trailing realized vol of daily log returns)
strategy vol_targeted_trend {
  env equities_1d
  uses features
  resolution @1d
  mode target

  # ma_bars: moving-average length in trading bars (200 bars = the 200-day average).
  # vol_bars: daily log returns behind the realized-vol estimate (not fixed by the task).
  # target_vol: annual volatility the long position is sized to (ten percent).
  # trading_days: bars per year, used to annualize the daily volatility.
  param ma_bars : Count = 200 in 50..300
  param vol_bars : Count = 20 in 10..60
  param target_vol : Scalar = 0.10 in 0.05..0.20
  param trading_days : Scalar = 252

  # Average of the last ma_bars closes, today's included; exists only once that many closes do.
  rel ma(+A: Equity, @T: Timestamp, -M: Price<USD>)
  ma(A, T, M) :- universe(A, T),
      M = mean(P) over (T1 in rows(T, ma_bars, min ma_bars), close(A, T1, P)).

  # Annualized realized volatility: sample std of the last vol_bars daily log returns.
  rel ann_vol(+A: Equity, @T: Timestamp, -V: Scalar)
  ann_vol(A, T, V) :- universe(A, T),
      S = std(R) over (T1 in rows(T, vol_bars, min vol_bars), logret(A, T1, R)),
      V = S * sqrt(trading_days).

  # Trend state. The complement is written by comparison, not `not`: close is
  # not declared complete, so `not above_ma` would be refused (WF-5).
  rel above_ma(-A: Equity, @T: Timestamp)
  above_ma(A, T) :- universe(A, T), close(A, T, P), ma(A, T, M), P > M.

  rel at_or_below_ma(-A: Equity, @T: Timestamp)
  at_or_below_ma(A, T) :- universe(A, T), close(A, T, P), ma(A, T, M), P <= M.

  # Long at the vol-targeted weight, capped at one. The cap is written 1.0:
  # with a bare integer 1 inside least the cap did not hold in the kernel.
  decide(T, target_weight(A, W)) :- above_ma(A, T), ann_vol(A, T, V),
      W = least(1.0, target_vol / V).
  # Flat otherwise.
  decide(T, target_weight(A, 0)) :- at_or_below_ma(A, T).
}
