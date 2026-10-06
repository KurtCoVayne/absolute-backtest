# Futures sessions (scripts/ingest/tradestation_sessions.py): for every root,
# the primary-session bar of each session day (@1d, labelled by its date) and
# the real minute prints an opening-range book reads and trades at (@1m,
# labelled at the minute's close). Prices are back-adjusted continuous
# contracts and may cross zero (asset_class future in the security table).
environment futures_sessions {
  session(-A: Equity, @T: Timestamp) @1d complete
  open_d(+A: Equity, @T: Timestamp, -O: Price<USD>) @1d complete
  high_d(+A: Equity, @T: Timestamp, -H: Price<USD>) @1d complete
  low_d(+A: Equity, @T: Timestamp, -L: Price<USD>) @1d complete
  close_d(+A: Equity, @T: Timestamp, -P: Price<USD>) @1d complete
  # Currency per point per contract (the security table's multiplier).
  multiplier(+A: Equity, @T: Timestamp, -M: Scalar) @1d complete
  # The reporting calendar (the lead market's session days).
  calendar(@T: Timestamp) @1d complete
  # The open of the session's first minute, when it traded; the close of the
  # last traded minute before minute 20 and before minute 30.
  open0_m(+A: Equity, @T: Timestamp, -P: Price<USD>) @1m complete
  close20_m(+A: Equity, @T: Timestamp, -P: Price<USD>) @1m complete
  close30_m(+A: Equity, @T: Timestamp, -P: Price<USD>) @1m complete
  # The prints a book trades at: the entry (first traded minute from minute
  # 30) and the exit (the session's last traded minute).
  open_m(+A: Equity, @T: Timestamp, -O: Price<USD>) @1m
  close_m(+A: Equity, @T: Timestamp, -P: Price<USD>) @1m
  # The decision time, 30 minutes after the open, of every session the book
  # may trade, with the session's open S.
  clock(-A: Equity, @T: Timestamp, -S: Timestamp) @1m complete
}
