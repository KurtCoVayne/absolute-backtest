# Tier-1 daily environment: the six primitives of the semantic model
# (section 6). `universe` is the complete entity domain; it is declared with
# `-A` so that rules can range over it.
environment equities_1d {
  close(+A: Equity, @T: Timestamp, -P: Price<USD>) @1d
  volume(+A: Equity, @T: Timestamp, -V: Quantity<Shares>) @1d
  universe(-A: Equity, @T: Timestamp) @1d complete
}
