# Tier-2 daily environment: adds bar extremes and an announcement relation
# whose ex-date is a value, not a temporal key (section 3). Exists so that
# the corpus can show a tier-1 strategy being refused a tier-2 primitive.
environment equities_1d_ext {
  close(+A: Equity, @T: Timestamp, -P: Price<USD>) @1d
  open(+A: Equity, @T: Timestamp, -O: Price<USD>) @1d
  high(+A: Equity, @T: Timestamp, -H: Price<USD>) @1d
  low(+A: Equity, @T: Timestamp, -L: Price<USD>) @1d
  volume(+A: Equity, @T: Timestamp, -V: Quantity<Shares>) @1d
  universe(-A: Equity, @T: Timestamp) @1d complete
  ticker(+A: Equity, @T: Timestamp, -S: Label) @1d complete
  dividend_announced(+A: Equity, @T: Timestamp, -Ex: Timestamp, -D: Price<USD>) @1d
}
