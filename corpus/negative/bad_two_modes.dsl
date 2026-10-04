# expect: C
# A strategy declares exactly one decision mode (WF-9): a second `mode` line
# is an error whichever order the two are written in, and the first one stays
# in effect (so `buy` under `mode target` is reported too).
strategy bad_two_modes {
  env equities_1d
  uses features
  resolution @1d
  mode target
  mode delta
  param qty : Quantity<Shares> = 100 shares
  decide(T, buy(A, qty)) :- universe(A, T), flat(A, T).
}
