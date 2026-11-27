# The catalog module for the equity class: what the catalog stores as traded,
# derived causally (B-3): the split factor and its cumulative product, the
# point-in-time adjusted close, the dividend going ex at a bar, total return,
# and the total-return index the executor can fill and mark at. Every
# recursion steps through the entity's own previous row.
library catalog {
  data { class equity   need bars   need actions   need listing }
  uses calendar
  rules {
    rel split_factor(A: Equity, @T, F: Scalar)
    split_factor(A, T, F) :- universe(A, T), F = case { split(A, T, F0) -> F0 ; else -> 1 }.

    rel cum_split(A: Equity, @T, C: Scalar)
    cum_split(A, T, C) :- first_row(A, T), split_factor(A, T, C).
    cum_split(A, T, C) :- prev_row(A, T, T1), cum_split(A, T1, C0), split_factor(A, T, F), C = C0 * F.

    # The as-traded close scaled by the splits at or before T: comparable across time.
    rel adj_close(A: Equity, @T, P: Price<USD>)
    adj_close(A, T, P) :- close(A, T, P0), cum_split(A, T, C), P = P0 * C.

    # The dividends going ex in T's bucket, announced at or before T: after the
    # calendar's previous bar and at or before T (3.3), so on a weekly calendar
    # every dividend of the week, as `split` there is the week's split; on the
    # base calendar the bar's own, and one announced on a bar and going ex on a
    # day with no bar at the next bar (E-145). The calendar's first bar has no
    # bucket start, and no return steps into it. On a calendar finer than the
    # actions' (a minute base) the window is refused: read it at the day.
    rel dividend_at(A: Equity, @T, D: Price<USD>)
    dividend_at(A, T, D) :- universe(A, T), prev(T, T1),
        D = sum(Amt) over (T0 in window[1y] min 1, dividend(A, T0, Ex, _, Amt), Ex > T1, Ex <= T).
    rel dividend_or_zero(A: Equity, @T, D: Price<USD>)
    dividend_or_zero(A, T, D) :- universe(A, T), D = case { dividend_at(A, T, D0) -> D0 ; else -> 0 USD/share }.

    # T's bucket's dividends as the total return takes them, L the entity's
    # last bar at or before T: G, the growth of a share that reinvests each
    # dividend announced in the bucket and going ex at an earlier bar of it
    # at the close of its ex-date; X, every other, reinvested at L's close,
    # the bucket's last price. `Ex < L` bounds the ex-date before T, so its
    # close is read causally (WF-6, E-163). A dividend going ex on a split's
    # day is held per share after the split, the vendor's amount over the
    # factor (the Norgate adapter's rescaling); times the day's factor it is
    # the vendor's amount, which the book adds to the day's close before
    # the split, (close + D) split (E-164). On the base calendar L is the
    # bar and G is 1.
    rel dividend_reinvested(A: Equity, @T, X: Price<USD>, G: Scalar)
    dividend_reinvested(A, T, X, 1.0) :- dividend_at(A, T, D), prev(T, T1), close@base(A, L, _) asof T,
        FL = case { split@base(A, L, F0) -> F0 ; else -> 1 },
        N = count(Ex) over (dividend(A, T, Ex, _, _), Ex > T1, Ex < L, close@base(A, Ex, _)), N = 0, X = D * FL.
    dividend_reinvested(A, T, X, G) :- dividend_at(A, T, D), prev(T, T1), close@base(A, L, _) asof T,
        FL = case { split@base(A, L, F0) -> F0 ; else -> 1 },
        S = sum(Amt) over (dividend(A, T, Ex, _, Amt), Ex > T1, Ex < L, close@base(A, Ex, _)),
        G = prod(1 + Amt * F / C) over (dividend(A, T, Ex, _, Amt), Ex > T1, Ex < L, close@base(A, Ex, C),
            F = case { split@base(A, Ex, F0) -> F0 ; else -> 1 }),
        X = (D - S) * FL.

    # Total return from the entity's previous row with each dividend
    # reinvested at the close of its ex-date, as the book compounds the
    # daily (close + D) split / previous close over the bucket (E-163):
    #   1 + R = (P + X) F / P0 x G.
    # On the base calendar it is the day's (P + D F) F / P0.
    rel total_return(A: Equity, @T, R: Scalar)
    total_return(A, T, R) :- close(A, T, P), not dividend_at(A, T, _), prev_row(A, T, T0), close(A, T0, P0),
        split_factor(A, T, F), R = P * F / P0 - 1.
    total_return(A, T, R) :- close(A, T, P), dividend_at(A, T, _), prev_row(A, T, T0), close(A, T0, P0),
        split_factor(A, T, F), dividend_reinvested(A, T, X, G), R = (P + X) * F / P0 * G - 1.
    rel log_return(A: Equity, @T, R: Scalar)
    log_return(A, T, R) :- total_return(A, T, R0), R = log(1 + R0).

    # The running total-return index, continuous across splits and dividends:
    # a price relation the execution block may fill and mark at.
    rel tr_index(A: Equity, @T, P: Price<USD>)
    tr_index(A, T, P) :- first_row(A, T), close(A, T, P).
    tr_index(A, T, P) :- prev_row(A, T, T0), tr_index(A, T0, P0), total_return(A, T, R), P = P0 * (1 + R).
  }
}
