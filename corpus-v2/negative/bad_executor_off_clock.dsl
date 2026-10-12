# expect: X
# `position` is supplied at the decision calendar (here the base, at the
# clock's bars); a rule on the session calendar reads it with a session
# label (WF-16). The book is read at the decision bar, where the kernel
# supplies it: `session_of(A, T, S)` then relates the bar to its session.
strategy bad_executor_off_clock {
  data { class future   need bars @1m   need sessions, point_value, commission   require tested   prefer latest }
  uses calendar
  calendars { session = sessions   es_days = sessions of "ES" }
  clock { decide on base at session + 30min }
  universe {
    instruments { ES from 2000-01-01; NQ from 2000-01-01 }
    universe(A, T) :- instruments(A, From), session_of(A, T, _), T >= From.
  }
  params { lot : Quantity<Contracts> = 1 contracts }
  rules {
    rel held_at_close(A: Future, @S: session, Q: Quantity<Contracts>)
    held_at_close(A, S, Q) :- session(A, S, _), position(A, S, Q).
  }
  decisions {
    silence flat
    orders moo_moc
    decide(T, hold(A, lot)) :- universe(A, T), session_of(A, T, S), prev(S, S1), held_at_close(A, S1, Q), Q = 0 contracts.
  }
  execution futures_fixed_base
  report { calendar es_days   periods 252 per year }
}
