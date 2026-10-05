# Dividend capture over the catalog environment: buy an index member once a
# dividend is announced and hold it through the ex-date. The announcement is
# read with the as-of join, so at T the strategy sees the latest announcement
# made at or before T and its ex-date as a value, never an announcement that
# had not yet been made.
strategy dividend_capture {
  env equities_1d_v2
  uses catalog
  resolution @1d
  mode delta

  param qty : Quantity<Shares> = 100 shares
  param index : Label = "SPX"

  # A dividend announced at or before T whose ex-date is still ahead.
  rel pending(+A: Equity, @T: Timestamp)
  pending(A, T) :- universe(A, T), dividend(A, _, Ex, _, _) asof T, Ex > T.

  decide(T, buy(A, qty)) :- universe(A, T), member(A, T, index), pending(A, T), flat(A, T).
  decide(T, sell(A, Q)) :- held(A, T, Q), not pending(A, T).
}
