# expect: F
# `M = mean(P1) over (close(A, T1, P1))` leaves the group's temporal key T1
# unconstrained: the mean runs over the whole sample, including bars after T,
# which is feature leakage (WF-6, aggregation groups). Every temporal key
# inside a group must be the head's T or bound by rows, window, prior_window,
# bucket or session of T.
strategy bad_full_sample_aggregate {
  data { class equity   need bars @1d   need listing   require reviewed   prefer latest }
  uses calendar, book
  clock { decide on base }
  universe { universe(A, T) :- listed(A, T), traded@base(A, T). }
  params { qty : Quantity<Shares> = 100 shares }
  rules {
    rel level(A: Equity, @T, Z: Scalar)
    level(A, T, Z) :- universe(A, T), close(A, T, P), M = mean(P1) over (close(A, T1, P1)), Z = P / M - 1.
  }
  decisions {
    silence hold
    orders market
    decide(T, trade(A, qty)) :- level(A, T, Z), Z < -0.1, flat(A, T).
  }
  execution equity_default
  report { calendar base   periods 252 per year }
}
