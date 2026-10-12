# The calendar module: the bars of the decision calendar and an entity's own
# rows, the bars at which it is in the universe (docs/language-v2.md, 7.1).
# `prev`, `lag`, `month_start`, `day_start` and `session_of` are builtins;
# everything here is a rule over them and the program's `universe`.
library calendar {
  data { class any }
  rules {
    rel bar(@T)
    bar(T) :- universe(_, T).

    # The entity's previous own row: the latest bar before T at which it was in the universe.
    rel prev_row(A: Entity, @T, T1: Timestamp)
    prev_row(A, T, T1) :- universe(A, T), prev(T, T0), universe(A, T1) asof T0.

    rel first_row(A: Entity, @T)
    first_row(A, T) :- universe(A, T), not prev_row(A, T, _).

    # 0 at the entity's first row, counting its own rows since.
    rel row_number(A: Entity, @T, K: Count)
    row_number(A, T, 0) :- first_row(A, T).
    row_number(A, T, K) :- prev_row(A, T, T1), row_number(A, T1, K0), K = K0 + 1.
  }
}
