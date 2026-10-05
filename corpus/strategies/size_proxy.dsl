# Canonical size, by the proxy the catalog has (data-bundle doc, section 8,
# stylized facts): market capitalisation is not a catalog relation, so
# average dollar volume stands in for it. At each month start, long the n
# index members with the smallest average dollar volume, short the n
# largest, four tenths of equity a leg.
strategy size_proxy {
  env equities_1d_v2
  uses catalog
  resolution @1d
  mode target

  param window : Duration = 60d in 20d..1y
  param min_obs : Count = 40
  param index : Label = "SPX"
  param n : Count = 1 in 1..100
  # Less than half of equity a leg: between rebalances the legs drift apart,
  # and the default policy halts a book whose gross exposure passes equity.
  param gross : Scalar = 0.4

  rel dollar_volume(-A: Equity, @T: Timestamp, -D: Notional<USD>)
  dollar_volume(A, T, D) :- universe(A, T), member(A, T, index), adv(A, T, window, min_obs, D).

  rel long_leg(-A: Equity, @T: Timestamp)
  long_leg(A, T) :- bar(T), month_start(T), top(n, dollar_volume(A, T, D), by (D asc, A asc)).

  rel short_leg(-A: Equity, @T: Timestamp)
  short_leg(A, T) :- bar(T), month_start(T), top(n, dollar_volume(A, T, D), by (D desc, A asc)).

  rel n_long(@T: Timestamp, -N: Count)
  n_long(T, N) :- bar(T), N = count(A) over (long_leg(A, T)), N > 0.

  rel n_short(@T: Timestamp, -N: Count)
  n_short(T, N) :- bar(T), N = count(A) over (short_leg(A, T)), N > 0.

  rel in_book(-A: Equity, @T: Timestamp)
  in_book(A, T) :- position(A, T, Q), Q > 0 shares.
  in_book(A, T) :- position(A, T, Q), Q < 0 shares.

  decide(T, target_weight(A, W)) :- long_leg(A, T), n_long(T, N), W = gross / N.
  decide(T, target_weight(A, W)) :- short_leg(A, T), n_short(T, N), W = -gross / N.
  decide(T, target_weight(A, 0)) :- bar(T), month_start(T), in_book(A, T), not long_leg(A, T), not short_leg(A, T).
}
