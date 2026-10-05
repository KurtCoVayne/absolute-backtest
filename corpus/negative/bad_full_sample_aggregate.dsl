# expect: F
# `M = mean(P1) over (close(A, T1, P1))` leaves the group's temporal key T1
# unconstrained: the mean runs over the whole sample, including bars after T,
# which is feature leakage (WF-6, aggregation groups; data-bundle doc,
# section 9 item 1). Every temporal key inside a group must be the head's T
# or bound by window / prior_window of T.
strategy bad_full_sample_aggregate {
  env equities_1d
  uses features
  resolution @1d
  mode delta
  param qty : Quantity<Shares> = 100 shares
  rel level(-A: Equity, @T: Timestamp, -Z: Scalar)
  level(A, T, Z) :- universe(A, T), close(A, T, P),
      M = mean(P1) over (close(A, T1, P1)), Z = P / M - 1.
  decide(T, buy(A, qty)) :- level(A, T, Z), Z < -0.1, flat(A, T).
}
