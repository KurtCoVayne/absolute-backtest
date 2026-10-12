# Bar-level identities for any class with price bars: the true range, the
# range, the gap from the entity's previous close, price returns (the catalog
# module's `total_return` is the return with actions applied), dollar volume
# and the liquidity floor over it. Every relation is calendar-polymorphic:
# `true_range@session` is the same rule on the session calendar.
library bars {
  data { class any   need bars }
  uses calendar
  rules {
    rel true_range(A: Entity, @T, V: Price<USD>)
    true_range(A, T, V) :- high(A, T, H), low(A, T, L), prev_row(A, T, T0), close(A, T0, C),
        V = greatest(H - L, abs(H - C), abs(L - C)).
    rel range(A: Entity, @T, V: Price<USD>)
    range(A, T, V) :- high(A, T, H), low(A, T, L), V = H - L.

    # The gap in price units (a future's prices may cross zero) and as a ratio.
    rel gap_points(A: Entity, @T, G: Price<USD>)
    gap_points(A, T, G) :- open(A, T, O), prev_row(A, T, T0), close(A, T0, C), G = O - C.
    rel gap(A: Entity, @T, G: Scalar)
    gap(A, T, G) :- open(A, T, O), prev_row(A, T, T0), close(A, T0, C), G = O / C - 1.

    rel price_return(A: Entity, @T, R: Scalar)
    price_return(A, T, R) :- close(A, T, P), prev_row(A, T, T0), close(A, T0, P0), R = P / P0 - 1.
    rel log_price_return(A: Entity, @T, R: Scalar)
    log_price_return(A, T, R) :- price_return(A, T, R0), R = log(1 + R0).

    # Price x Quantity = Notional by dimension (F-9).
    rel dollar_volume(A: Entity, @T, D: Notional<USD>)
    dollar_volume(A, T, D) :- close(A, T, P), volume(A, T, V), D = P * V.

    # Average dollar volume over the entity's last N rows, and a liquidity floor over twenty rows.
    rel adv[N: Count](A: Entity, @T, D: Notional<USD>)
    adv[N](A, T, D) :- universe(A, T), D = mean(X) over (T1 in rows[N], dollar_volume(A, T1, X)).
    rel liquid[Floor: Notional<USD>](A: Entity, @T)
    liquid[Floor](A, T) :- adv[20](A, T, D), D >= Floor.
  }
}
