# Classic pair: hedge ratio by OLS over log returns, z-score of the residual
# spread. `corr`-style bivariate aggregates align the two series on T1 by
# construction (section 4, bivariate aggregates).
# allow: W6  (the ticker literal is a snapshot of the bundle date)
strategy pairs_trading {
  env equities_1d
  uses features
  resolution @1d
  mode delta

  param y : Equity = "AAA"
  param x : Equity = "BBB"
  param lb : Duration = 60d in 30d..250d
  param lb_min : Count = 40
  param entry : Scalar = 2.0
  param exit : Scalar = 0.5
  param qty : Quantity<Shares> = 100 shares

  rel beta(@T: Timestamp, -B: Scalar)
  beta(T, B) :- bar(T),
      B = ols_beta(RY, RX) over (logret(y, T1, RY), logret(x, T1, RX), T1 in window(T, lb, min lb_min)).

  rel spread(@T: Timestamp, -S: Scalar)
  spread(T, S) :- beta(T, B), logret(y, T, RY), logret(x, T, RX), S = RY - B * RX.

  rel spread_z(@T: Timestamp, -Z: Scalar)
  spread_z(T, Z) :- spread(T, S),
      M = mean(S1) over (T1 in window(T, lb, min lb_min), spread(T1, S1)),
      D = std(S1) over (T1 in window(T, lb, min lb_min), spread(T1, S1)),
      Z = (S - M) / D.

  decide(T, short(y, qty)) :- spread_z(T, Z), Z > entry, flat(y, T), flat(x, T).
  decide(T, buy(x, qty)) :- spread_z(T, Z), Z > entry, flat(y, T), flat(x, T).
  decide(T, buy(y, qty)) :- spread_z(T, Z), Z < -entry, flat(y, T), flat(x, T).
  decide(T, short(x, qty)) :- spread_z(T, Z), Z < -entry, flat(y, T), flat(x, T).

  decide(T, cover(y, Q)) :- spread_z(T, Z), abs(Z) < exit, position(y, T, Q0), Q0 < 0 shares, Q = -Q0.
  decide(T, sell(x, Q)) :- spread_z(T, Z), abs(Z) < exit, position(x, T, Q), Q > 0 shares.
  decide(T, sell(y, Q)) :- spread_z(T, Z), abs(Z) < exit, position(y, T, Q), Q > 0 shares.
  decide(T, cover(x, Q)) :- spread_z(T, Z), abs(Z) < exit, position(x, T, Q0), Q0 < 0 shares, Q = -Q0.
}
