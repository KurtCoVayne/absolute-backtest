# The option class: listed equity and index options, each contract an
# instrument with its terms in the registry's `option_spec` (the kernel's
# `spec`), quoted at the base calendar. A program's `need chain` names the
# contracts it reads (the kernel's `chain`); the underlyings' bars and
# actions are the equity class's.
class option {
  base @1d                       # end-of-day snapshots; an intraday source is a finer base
  unit contracts

  # Quotes as snapshotted, and the margin a source publishes per contract.
  quote(A: Option, @T, Bid: Price<USD>, Ask: Price<USD>, BidSize: Quantity<Contracts>, AskSize: Quantity<Contracts>, Last: Price<USD>, Volume: Quantity<Contracts>, OI: Quantity<Contracts>)
  # greeks(A: Option, @T, IV: Scalar, Delta: Scalar, Gamma: Scalar, Theta: Scalar, Vega: Scalar, Rho: Scalar)   # the kernel's relation (K-2.4): these rows under `greeks data`, the model's under `greeks model`
  margin(A: Option, @T, Initial: Notional<USD>, Maintenance: Notional<USD>) complete   # per contract, where the source publishes it (K-5.6)

  # The underlyings: the equity class's bars and actions.
  open(A: Equity, @T, O: Price<USD>)
  high(A: Equity, @T, H: Price<USD>)
  low(A: Equity, @T, L: Price<USD>)
  close(A: Equity, @T, P: Price<USD>)
  volume(A: Equity, @T, V: Quantity<Shares>)
  split(A: Equity, @T, Factor: Scalar) complete                     # effective at the ex-date T: new shares per old
  dividend(A: Equity, @T, Ex: Timestamp, Pay: Timestamp, Amount: Price<USD>) complete   # announced at T
  special_dividend(A: Equity, @T, Ex: Timestamp, Amount: Price<USD>) complete   # a dividend OCC adjusts strikes for (K-5.5.5)
  delisted(A: Equity, @T, Reason: Label) complete                   # a status from the delisting bar on

  # Vocabularies of the kernel's `spec` (K-2.4).
  rights      "call", "put"
  styles      "american", "european"
  settlements "physical", "cash"
}
