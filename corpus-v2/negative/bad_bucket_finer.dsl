# expect: X
# A bar view aggregates a relation to a coarser calendar; `value@base` of the
# weekly VIX asks for a finer one. On a daily rule the weekly series is read
# as of the bar, `value("VIX", T, V)`, and the checker picks the direction
# from the calendars (3.4).
strategy bad_bucket_finer {
  data { class equity   need bars @1d   need listing   need series "VIX" @1w   require reviewed   prefer latest }
  uses calendar, book
  clock { decide on base }
  universe { universe(A, T) :- listed(A, T), traded@base(A, T). }
  params { qty : Quantity<Shares> = 100 shares   calm : Scalar = 20 }
  rules {
    rel quiet(@T)
    quiet(T) :- bar(T), value@base("VIX", T, V), V < calm.
  }
  decisions {
    silence hold
    orders market
    decide(T, trade(A, qty)) :- quiet(T), universe(A, T), flat(A, T).
  }
  execution equity_default
  report { calendar base   periods 252 per year }
}
