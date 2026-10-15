# Fade a gap down at the open. Not expressible on a daily calendar (section
# 6, availability convention): the strategy decides on the minute base, where
# the first bar of the day is available at its own close and the prior close
# is the bar before it.
strategy opening_gap {
  data {
    class   equity
    need    bars @1m
    need    actions, listing
    require tested
    prefer  latest
  }
  uses  calendar, book
  calendars { daily = days }
  clock { decide on base }
  universe {
    # A name's rows are the minutes it traded; the listing is a daily fact, read as of the minute.
    universe(A, T) :- listed(A, T), traded@base(A, T).
  }

  params {
    gap : Scalar = 0.02 in 0.005..0.1
    qty : Quantity<Shares> = 100 shares
  }

  rules {
    # The first minute of the day against the last minute of the day before.
    rel gap_down(A: Equity, @T, G: Scalar)
    gap_down(A, T, G) :- universe(A, T), day_start(T), close(A, T, P),
        prev(T, T0), close(A, T0, P0), G = P / P0 - 1, G < -gap.
  }

  decisions {
    silence hold
    orders  market
    decide(T, trade(A, qty)) :- gap_down(A, T, _), flat(A, T).
    decide(T, trade(A, Q)) :- held(A, T, Q0), day_start(T), Q = -Q0.
  }

  execution equity_default
  report { calendar daily   periods 252 per year }
}
