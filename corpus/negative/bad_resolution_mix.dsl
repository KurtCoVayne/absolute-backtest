# expect: X
# `close_m` is at @1m and this rule is at @1d; two resolutions meet only
# through resample (WF-10).
strategy bad_resolution_mix {
  env equities_1m
  uses bars
  resolution @1d
  mode target
  param n : Count = 2
  rel last_price(-A: Equity, @T: Timestamp, -P: Price<USD>)
  last_price(A, T, P) :- universe_d(A, T), close_m(A, T, P).
  rel chosen(-A: Equity, @T: Timestamp)
  chosen(A, T) :- bar_d(T), top(n, last_price(A, T, P), by (P desc, A asc)).
  decide(T, target_weight(A, 0.5)) :- chosen(A, T).
}
