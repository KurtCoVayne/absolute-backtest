# Keyed temporal recursions written once (F-4.6, docs/language-v2.md 7.1):
# the latch, bars held, the entry price and the high since entry. Every
# recursion steps through `prev` or `prev_row`, and every pair of rules for
# one relation is guarded by complementary literals, so the relations are
# keyed by the syntactic condition.
library state {
  data { class any }
  uses calendar, book
  rules {
    # In the latch at T when it may stay and either enters at T or was in it at its previous own row.
    rel latch[enter: rel(Entity, @T), stay: rel(Entity, @T)](A: Entity, @T)
    latch[enter, stay](A, T) :- universe(A, T), enter(A, T).
    latch[enter, stay](A, T) :- universe(A, T), not enter(A, T), stay(A, T), prev_row(A, T, T1), latch[enter, stay](A, T1).

    # Bars since the position was opened: 0 at the first held bar.
    rel bars_held(A: Entity, @T, N: Count)
    bars_held(A, T, 0) :- in_book(A, T), prev(T, T0), not in_book(A, T0).
    bars_held(A, T, N) :- in_book(A, T), prev(T, T0), bars_held(A, T0, N0), N = N0 + 1.

    # The price of the fill that opened the position, carried while it is held.
    rel entry_price(A: Entity, @T, P: Price<USD>)
    entry_price(A, T, P) :- fill(A, T, _, P), prev(T, T0), not in_book(A, T0).
    entry_price(A, T, P) :- in_book(A, T), not fill(A, T, _, _), prev(T, T0), entry_price(A, T0, P).

    # The highest close since entry, the entry's fill price first.
    rel high_since_entry(A: Entity, @T, H: Price<USD>)
    high_since_entry(A, T, H) :- fill(A, T, Q, P), Q > 0 units, H = P.
    high_since_entry(A, T, H) :- held(A, T, _), not fill(A, T, _, _), prev(T, T0), high_since_entry(A, T0, H0), close(A, T, P), H = greatest(H0, P).
  }
}
