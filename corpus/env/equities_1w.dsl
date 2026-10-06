# Weekly catalog environment (scripts/ingest/ndlake_weekly.py): one bar per
# market week, keyed at the week's last trading day, at the @1d resolution (a
# time domain of week ends, so prev and rows step a week). Raw facts only:
# `close` is the asset's last raw close of the week, `trclose` the running
# total-return index the executor fills and marks on (--price-relation trclose
# --actions in-prices), `split` the week's split factor, `dvol` the week's mean
# daily dollar volume; `series` holds market series by name (SPXTR, VIX,
# VIX_MA4).
environment equities_1w {
  # Every universe row carries a close, a total-return price and a dollar
  # volume; a week without a series reading has none (complete: missing is false).
  close(+A: Equity, @T: Timestamp, -P: Price<USD>) @1d complete
  trclose(+A: Equity, @T: Timestamp, -P: Price<USD>) @1d complete
  split(+A: Equity, @T: Timestamp, -Factor: Scalar) @1d complete
  dvol(+A: Equity, @T: Timestamp, -D: Notional<USD>) @1d complete
  universe(-A: Equity, @T: Timestamp) @1d complete
  member(+A: Equity, @T: Timestamp, +Idx: Label) @1d complete
  delisted(-A: Equity, @T: Timestamp, -Reason: Label) @1d complete
  series(@T: Timestamp, +Name: Label, -V: Scalar) @1d complete
}
