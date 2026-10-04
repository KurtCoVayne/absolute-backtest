# Standard daily bars from minute closes (section 4, resampling semantics).
# Each bucket needs at least 300 minute bars; a short session yields no bar
# in open_d/close_d/high_d/low_d/volume_d. It still yields universe_d and
# bar_d (min 1) and stays in the @1d time domain (section 6): prev from the
# next day lands on it, and the executor fills there at its last minute
# close. `min K` removes a bar from a relation, never a day from the domain.
library bars {
  env equities_1m
  resolution @1d

  rel universe_d(-A: Equity, @T: Timestamp)
  universe_d(A, T) :- resample(universe_m(A, T1) to @1d as T, min 1, N = count(T1)).

  rel bar_d(@T: Timestamp)
  bar_d(T) :- universe_d(_, T).

  rel open_d(+A: Equity, @T: Timestamp, -O: Price<USD>)
  open_d(A, T, O) :- resample(close_m(A, T1, P) to @1d as T, min 300, O = first(P)).

  rel close_d(+A: Equity, @T: Timestamp, -C: Price<USD>)
  close_d(A, T, C) :- resample(close_m(A, T1, P) to @1d as T, min 300, C = last(P)).

  rel high_d(+A: Equity, @T: Timestamp, -H: Price<USD>)
  high_d(A, T, H) :- resample(close_m(A, T1, P) to @1d as T, min 300, H = max(P)).

  rel low_d(+A: Equity, @T: Timestamp, -L: Price<USD>)
  low_d(A, T, L) :- resample(close_m(A, T1, P) to @1d as T, min 300, L = min(P)).

  rel volume_d(+A: Equity, @T: Timestamp, -V: Quantity<Shares>)
  volume_d(A, T, V) :- resample(volume_m(A, T1, Q) to @1d as T, min 300, V = sum(Q)).
}
