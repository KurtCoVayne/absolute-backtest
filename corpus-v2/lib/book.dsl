# Views of the executor's relations: what the book holds, the current weight
# of a holding, exposure, the book's return, peak and drawdown, a trailing
# volatility, and the cooldown test over the strategy's own decisions. Every
# relation here is closed (F-6), so it may be negated.
library book {
  data { class any }
  uses calendar
  rules {
    rel held(A: Entity, @T, Q: Quantity<Unit>)
    held(A, T, Q) :- position(A, T, Q), Q > 0 units.
    rel short(A: Entity, @T, Q: Quantity<Unit>)
    short(A, T, Q) :- position(A, T, Q), Q < 0 units.
    rel in_book(A: Entity, @T)
    in_book(A, T) :- held(A, T, _).
    in_book(A, T) :- short(A, T, _).
    rel flat(A: Entity, @T)
    flat(A, T) :- universe(A, T), not position(A, T, _).

    # The holding's weight of the capital the execution block names, at the mark.
    rel weight_now(A: Entity, @T, W: Scalar)
    weight_now(A, T, W) :- position(A, T, Q), mark(A, T, P), capital(T, C), W = Q * P / C.

    rel exposure(@T, G: Notional<USD>)
    exposure(T, G) :- bar(T), G = sum(abs(Q) * P) over (position(A, T, Q), mark(A, T, P)) within (T).

    rel nav_return(@T, R: Scalar)
    nav_return(T, R) :- nav(T, N), prev(T, T0), nav(T0, N0), R = N / N0 - 1.
    rel peak_nav(@T, P: Notional<USD>)
    peak_nav(T, P) :- nav(T, N), P = case { prev(T, T0), peak_nav(T0, P0), P0 > N -> P0 ; else -> N }.
    rel drawdown(@T, D: Scalar)
    drawdown(T, D) :- nav(T, N), peak_nav(T, P), D = 1 - N / P.
    rel nav_vol[N: Count](@T, V: Scalar)
    nav_vol[N](T, V) :- nav(T, _), V = std(R) over (T1 in rows[N], nav_return(T1, R)).

    # A buy was decided for A within the last W of history.
    rel bought_within[W: Duration](A: Entity, @T)
    bought_within[W](A, T) :- universe(A, T),
        C = count(T0) over (T0 in prior_window[W] min 1, decided(T0, trade(A, Q)), Q > 0 units), C > 0.
  }
}
