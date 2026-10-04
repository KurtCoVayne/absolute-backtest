# expect: U
# `rsi` is declared nowhere: not in the strategy, its libraries or the environment.
strategy bad_undeclared {
  env equities_1d
  uses features
  resolution @1d
  mode delta
  param qty : Quantity<Shares> = 100 shares
  decide(T, buy(A, qty)) :- universe(A, T), rsi(A, T, 14d, R), R < 30, flat(A, T).
}
