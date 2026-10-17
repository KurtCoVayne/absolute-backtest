# The equity class, as `abt bundle env` generates it from a bundle's manifest
# (docs/language-v2.md, 3.2 and 3.6): the primitives at the base calendar,
# the catalog relations, and the vocabularies. A program never names this
# file; its `data` block names what it needs and the kernel resolves it.
class equity {
  base @1d                       # the finest resolution this bundle holds; a @1m bundle derives @1d by the bar views
  unit shares

  # Bars as traded.
  open(A: Equity, @T, O: Price<USD>)
  high(A: Equity, @T, H: Price<USD>)
  low(A: Equity, @T, L: Price<USD>)
  close(A: Equity, @T, P: Price<USD>)
  volume(A: Equity, @T, V: Quantity<Shares>)

  # The catalog: listing and identity, corporate actions as events, point-in-time lists.
  listed(A: Equity, @T) complete                                    # listed and tradable on T
  ticker(A: Equity, @T, S: Label) complete
  split(A: Equity, @T, Factor: Scalar) complete                     # effective at the ex-date T: new shares per old
  dividend(A: Equity, @T, Ex: Timestamp, Pay: Timestamp, Amount: Price<USD>) complete   # announced at T
  special_dividend(A: Equity, @T, Ex: Timestamp, Amount: Price<USD>) complete   # a dividend OCC adjusts strikes for (K-5.5.5)
  delisted(A: Equity, @T, Reason: Label) complete                   # a status from the delisting bar on
  member(A: Equity, @T, List: Label) complete
  classification(A: Equity, @T, Scheme: Label, Code: Label) complete
  margin(A: Equity, @T, Initial: Notional<USD>, Maintenance: Notional<USD>) complete   # per share, where the source publishes it (K-5.6)

  # Vocabularies (WF-15 checks a program's labels against them).
  lists   "SPX", "SP500", "SP400", "SP600", "ETF"
  reasons "bankruptcy", "acquisition", "voluntary", "regulatory"
  schemes "sector"
}
