# expect: Q
# The data block is a conjunctive query over the catalog and needs at least
# one `need` (WF-17); with none, nothing binds the primitives the rules read.
strategy bad_demand_empty {
  data { class equity   require tested   prefer latest }
  uses calendar, book
  clock { decide on base }
  universe { universe(A, T) :- listed(A, T), traded@base(A, T). }
  params { qty : Quantity<Shares> = 100 shares }
  rules { }
  decisions {
    silence hold
    orders market
    decide(T, trade(A, qty)) :- universe(A, T), close(A, T, _), flat(A, T).
  }
  execution equity_default
  report { calendar base   periods 252 per year }
}
