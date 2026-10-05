# Daily catalog environment (data-bundle doc, section 3): prices as traded,
# corporate actions as events, point-in-time membership and status, and the
# security table's ticker relation. Nothing here is adjusted with hindsight:
# the `catalog` library derives total return and a point-in-time adjusted
# close from the actions, causally.
environment equities_1d_v2 {
  open(+A: Equity, @T: Timestamp, -O: Price<USD>) @1d
  high(+A: Equity, @T: Timestamp, -H: Price<USD>) @1d
  low(+A: Equity, @T: Timestamp, -L: Price<USD>) @1d
  close(+A: Equity, @T: Timestamp, -P: Price<USD>) @1d
  volume(+A: Equity, @T: Timestamp, -V: Quantity<Shares>) @1d
  # Listed and tradable on T (the complete entity domain).
  universe(-A: Equity, @T: Timestamp) @1d complete
  # The ticker A carried over bar T, from the security table.
  ticker(+A: Equity, @T: Timestamp, -S: Label) @1d complete
  # A split effective at the ex-date T: Factor new shares per old share.
  split(+A: Equity, @T: Timestamp, -Factor: Scalar) @1d complete
  # A cash dividend announced at T, with its ex-date, pay date and amount per share.
  dividend(+A: Equity, @T: Timestamp, -Ex: Timestamp, -Pay: Timestamp, -Amount: Price<USD>) @1d complete
  # Delisted on or before T, with the reason: a status, present at the
  # delisting bar and every later bar the data covers.
  delisted(-A: Equity, @T: Timestamp, -Reason: Label) @1d complete
  # Point-in-time index membership and classification.
  member(+A: Equity, @T: Timestamp, +Idx: Label) @1d complete
  classification(+A: Equity, @T: Timestamp, +Scheme: Label, -Code: Label) @1d complete
}
