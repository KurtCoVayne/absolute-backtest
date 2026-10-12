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

    # The dividend going ex at T, announced at or before T.
    rel dividend_at(A: Equity, @T, D: Price<USD>)
    dividend_at(A, T, D) :- universe(A, T), D = sum(Amt) over (T0 in window[1y] min 1, dividend(A, T0, Ex, _, Amt), Ex = T).
    rel dividend_or_zero(A: Equity, @T, D: Price<USD>)
    dividend_or_zero(A, T, D) :- universe(A, T), D = case { dividend_at(A, T, D0) -> D0 ; else -> 0 USD/share }.

    # Total return from the entity's previous row: (P F + D) / P0 - 1.
    rel total_return(A: Equity, @T, R: Scalar)
    total_return(A, T, R) :- close(A, T, P), prev_row(A, T, T0), close(A, T0, P0),
        split_factor(A, T, F), dividend_or_zero(A, T, D), R = (P * F + D) / P0 - 1.
    rel log_return(A: Equity, @T, R: Scalar)
    log_return(A, T, R) :- total_return(A, T, R0), R = log(1 + R0).

    # The running total-return index, continuous across splits and dividends:
    # a price relation the execution block may fill and mark at.
    rel tr_index(A: Equity, @T, P: Price<USD>)
    tr_index(A, T, P) :- first_row(A, T), close(A, T, P).
    tr_index(A, T, P) :- prev_row(A, T, T0), tr_index(A, T0, P0), total_return(A, T, R), P = P0 * (1 + R).
  }
}
