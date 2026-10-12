# Cross-sectional operators over the entities of one bar, each with its group
# written (WF-13): the decile of a value, its cross-sectional z-score, and the
# share of entities above a threshold.
library xsec {
  data { class any }
  uses calendar
  rules {
    # ceil(rank / n x 10), ties sharing the average rank, 1 the smallest tenth.
    rel decile[R: rel(Entity, @T, D)](A: Entity, @T, K: Count)
    decile[R](A, T, K) :- Rk = rank_average(R(A, T, X) by (X asc)) within (T),
        N = count(A1) over (R(A1, T, _)) within (T), K = decile(Rk, N).

    rel zscore_xs[R: rel(Entity, @T, D)](A: Entity, @T, Z: Scalar)
    zscore_xs[R](A, T, Z) :- R(A, T, X),
        M = mean(X1) over (R(A1, T, X1)) within (T),
        S = std(X1) over (R(A1, T, X1)) within (T),
        Z = (X - M) / S.

    # Of the entities R holds at T, the share whose value is above Th; none at all yields no tuple.
    rel share_above[R: rel(Entity, @T, D), Th: D](@T, S: Scalar)
    share_above[R, Th](T, S) :- bar(T), N = count(A) over (R(A, T, _)) within (T), N > 0,
        C = count(A) over (R(A, T, X), X > Th) within (T), S = C / N.
  }
}
