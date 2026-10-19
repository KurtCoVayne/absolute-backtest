# The future class: back-adjusted continuous contracts per root, whose prices
# may cross zero, with each root's own sessions, point value and commission
# per contract per side in the catalog.
class future {
  base @1m
  unit contracts

  open(A: Future, @T, O: Price<USD>)
  high(A: Future, @T, H: Price<USD>)
  low(A: Future, @T, L: Price<USD>)
  close(A: Future, @T, P: Price<USD>)
  volume(A: Future, @T, V: Quantity<Contracts>)

  session(A: Future, @S, Open: Timestamp) complete                  # one per root and session, labelled at the session's close
  point_value(A: Future, @T, V: Scalar) complete                    # currency per point per contract
  commission(A: Future, @T, C: Notional<USD>) complete              # per contract per side
  member(A: Future, @T, List: Label) complete
  usable(A: Future, @T) complete                                    # a research-defined availability, from the bundle's review

  # Settlement, open interest and the margin an exchange publishes (K-5.4, K-5.6).
  settlement(A: Future, @T, P: Price<USD>)                          # each session's settlement price, back-adjusted as the bars
  open_interest(A: Future, @T, N: Quantity<Contracts>)
  margin(A: Future, @T, Initial: Notional<USD>, Maintenance: Notional<USD>) complete   # per contract

  lists "HOME-26", "HOLD-6"
}
