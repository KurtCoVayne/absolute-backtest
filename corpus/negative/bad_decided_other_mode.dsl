# expect: C
# `decided` holds only this strategy's own decisions (section 3), and a
# target-mode strategy can only ever emit target constructors, so a pattern
# with a delta constructor can never match (WF-9).
strategy bad_decided_other_mode {
  env equities_1d
  uses features
  resolution @1d
  mode target
  decide(T, target_weight(A, 0.5)) :- universe(A, T), prev(T, T0), decided(T0, buy(A, _)).
}
