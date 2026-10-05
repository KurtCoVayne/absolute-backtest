# Metrics written in the DSL over the executor's `nav` (data-bundle doc,
# section 7): the per-bar return of the book, its running peak and drawdown,
# and a trailing volatility. Each is a relation at the decision resolution,
# so a strategy can read its own drawdown or volatility and act on it, and
# the study checks it like any other rule. Statistics the language cannot
# express (deflated Sharpe, PBO, bootstrap intervals) live in the kernel's
# metrics library and read the same series.
library metrics {
  env equities_1d
  resolution @1d

  # The book's simple return over the bar ending at T.
  rel nav_ret(@T: Timestamp, -R: Scalar)
  nav_ret(T, R) :- nav(T, N), prev(T, T1), nav(T1, N1), R = N / N1 - 1.

  # The highest NAV at or before T.
  rel peak_nav(@T: Timestamp, -P: Notional<USD>)
  peak_nav(T, P) :- nav(T, _), P = max(N1) over (T1 in window(T, 100y, min 1), nav(T1, N1)).

  # The drawdown from the running peak at T (0 at a new high).
  rel drawdown(@T: Timestamp, -D: Scalar)
  drawdown(T, D) :- nav(T, N), peak_nav(T, P), D = 1 - N / P.

  # The standard deviation of the book's returns over the trailing window.
  rel nav_vol(@T: Timestamp, +N: Duration, +K: Count, -V: Scalar)
  nav_vol(T, N, K, V) :- nav(T, _), V = std(R) over (T1 in window(T, N, min K), nav_ret(T1, R)).
}
