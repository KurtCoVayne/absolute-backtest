# expect: T
# A parameter's default must lie within its declared range (section 3):
# 400d is outside 5d..250d.
strategy bad_param_out_of_range {
  env equities_1d
  uses features
  resolution @1d
  mode delta
  param qty : Quantity<Shares> = 100 shares
  param lb : Duration = 400d in 5d..250d
  rel m(-A: Equity, @T: Timestamp, -M: Price<USD>)
  m(A, T, M) :- universe(A, T), M = mean(P) over (T1 in window(T, lb, min 1), close(A, T1, P)).
  decide(T, buy(A, qty)) :- m(A, T, _), flat(A, T).
}
