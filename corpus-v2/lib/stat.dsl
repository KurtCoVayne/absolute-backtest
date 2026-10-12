# Rolling and recursive statistics over an entity's own rows or a calendar
# window, for any relation of the shape (entity, key, value) and any
# dimension: `mean_rows[20, close]` is a 20-row mean of prices,
# `std_window[60d, 40, log_return]` a 60-day standard deviation of returns.
# The aggregate classes of F-8 decide each one's online cost: the moments are
# algebraic, max and min distributive, the median holistic.
library stat {
  data { class any }
  uses calendar
  rules {
    rel mean_rows[N: Count, R: rel(Entity, @T, D)](A: Entity, @T, M: D)
    mean_rows[N, R](A, T, M) :- universe(A, T), M = mean(X) over (T1 in rows[N], R(A, T1, X)).
    rel sum_rows[N: Count, R: rel(Entity, @T, D)](A: Entity, @T, S: D)
    sum_rows[N, R](A, T, S) :- universe(A, T), S = sum(X) over (T1 in rows[N], R(A, T1, X)).
    rel std_rows[N: Count, R: rel(Entity, @T, D)](A: Entity, @T, S: D)
    std_rows[N, R](A, T, S) :- universe(A, T), S = std(X) over (T1 in rows[N], R(A, T1, X)).
    rel max_rows[N: Count, R: rel(Entity, @T, D)](A: Entity, @T, H: D)
    max_rows[N, R](A, T, H) :- universe(A, T), H = max(X) over (T1 in rows[N], R(A, T1, X)).
    rel min_rows[N: Count, R: rel(Entity, @T, D)](A: Entity, @T, L: D)
    min_rows[N, R](A, T, L) :- universe(A, T), L = min(X) over (T1 in rows[N], R(A, T1, X)).
    rel median_rows[N: Count, K: Count, R: rel(Entity, @T, D)](A: Entity, @T, Q: D)
    median_rows[N, K, R](A, T, Q) :- universe(A, T), Q = median(X) over (T1 in rows[N] min K, R(A, T1, X)).

    rel mean_window[W: Duration, K: Count, R: rel(Entity, @T, D)](A: Entity, @T, M: D)
    mean_window[W, K, R](A, T, M) :- universe(A, T), M = mean(X) over (T1 in window[W] min K, R(A, T1, X)).
    rel sum_window[W: Duration, K: Count, R: rel(Entity, @T, D)](A: Entity, @T, S: D)
    sum_window[W, K, R](A, T, S) :- universe(A, T), S = sum(X) over (T1 in window[W] min K, R(A, T1, X)).
    rel std_window[W: Duration, K: Count, R: rel(Entity, @T, D)](A: Entity, @T, S: D)
    std_window[W, K, R](A, T, S) :- universe(A, T), S = std(X) over (T1 in window[W] min K, R(A, T1, X)).
    rel max_window[W: Duration, K: Count, R: rel(Entity, @T, D)](A: Entity, @T, H: D)
    max_window[W, K, R](A, T, H) :- universe(A, T), H = max(X) over (T1 in window[W] min K, R(A, T1, X)).
    rel min_window[W: Duration, K: Count, R: rel(Entity, @T, D)](A: Entity, @T, L: D)
    min_window[W, K, R](A, T, L) :- universe(A, T), L = min(X) over (T1 in window[W] min K, R(A, T1, X)).
    # The window before T, T excluded: the prior high of a breakout.
    rel max_prior_window[W: Duration, K: Count, R: rel(Entity, @T, D)](A: Entity, @T, H: D)
    max_prior_window[W, K, R](A, T, H) :- universe(A, T), H = max(X) over (T1 in prior_window[W] min K, R(A, T1, X)).
    rel mean_prior_window[W: Duration, K: Count, R: rel(Entity, @T, D)](A: Entity, @T, M: D)
    mean_prior_window[W, K, R](A, T, M) :- universe(A, T), M = mean(X) over (T1 in prior_window[W] min K, R(A, T1, X)).

    # The value's distance from its rolling mean in rolling standard deviations.
    rel zscore_rows[N: Count, R: rel(Entity, @T, D)](A: Entity, @T, Z: Scalar)
    zscore_rows[N, R](A, T, Z) :- R(A, T, X), mean_rows[N, R](A, T, M), std_rows[N, R](A, T, S), Z = (X - M) / S.
    rel zscore_window[W: Duration, K: Count, R: rel(Entity, @T, D)](A: Entity, @T, Z: Scalar)
    zscore_window[W, K, R](A, T, Z) :- R(A, T, X), mean_window[W, K, R](A, T, M), std_window[W, K, R](A, T, S), Z = (X - M) / S.

    # Recursions over the entity's own rows: keyed (F-4.6), one value a row.
    rel cumsum[R: rel(Entity, @T, D)](A: Entity, @T, S: D)
    cumsum[R](A, T, S) :- first_row(A, T), R(A, T, S).
    cumsum[R](A, T, S) :- prev_row(A, T, T0), cumsum[R](A, T0, S0), R(A, T, X), S = S0 + X.
    rel cumprod[R: rel(Entity, @T, Scalar)](A: Entity, @T, P: Scalar)
    cumprod[R](A, T, P) :- first_row(A, T), R(A, T, P).
    cumprod[R](A, T, P) :- prev_row(A, T, T0), cumprod[R](A, T0, P0), R(A, T, X), P = P0 * X.
    rel ewm[L: Scalar, R: rel(Entity, @T, D)](A: Entity, @T, E: D)
    ewm[L, R](A, T, E) :- first_row(A, T), R(A, T, E).
    ewm[L, R](A, T, E) :- prev_row(A, T, T0), ewm[L, R](A, T0, E0), R(A, T, X), E = L * X + (1 - L) * E0.
  }
}
