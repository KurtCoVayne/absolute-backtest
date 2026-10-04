# expect: U
# `prev` is a temporal builtin (section 4); a relation may not take the name
# of a builtin or a keyword, and the rules that mention it are not judged.
strategy bad_reserved_name {
  env equities_1d
  uses features
  resolution @1d
  mode delta
  param qty : Quantity<Shares> = 100 shares
  rel prev(-A: Equity, @T: Timestamp)
  prev(A, T) :- universe(A, T).
  decide(T, buy(A, qty)) :- prev(A, T), flat(A, T).
}
