# expect: F
# `session_of(A, T, S)` binds the label of T's own session, which is the
# session's close and lies after T: S is a value, not a causal key (3.5). A
# rule at a base bar may read the previous session's features through
# `prev(S, S1)`, never the current session's.
strategy bad_session_own_label {
  data { class future   need bars @1m   need sessions, point_value, commission   require reviewed   prefer latest }
  uses calendar, bars
  calendars { session = sessions   es_days = sessions of "ES" }
  clock { decide on base at session + 30min }
  universe {
    instruments { ES from 2000-01-01; NQ from 2000-01-01 }
    universe(A, T) :- instruments(A, From), session_of(A, T, _), T >= From.
  }
  params { risk : Notional<USD> = 5_000 USD }
  rules {
    rel atr(A: Future, @S: session, V: Price<USD>)
    atr(A, S, V) :- session(A, S, _), V = mean(R) over (S1 in rows[20], true_range@session(A, S1, R)).
    rel signal(A: Future, @T, V: Price<USD>)
    signal(A, T, V) :- universe(A, T), session_of(A, T, S), atr(A, S, V), V > 0 USD/contract.
  }
  decisions {
    silence flat
    orders moo_moc
    decide(T, hold(A, N)) :- signal(A, T, V), point_value(A, T, PV), N = risk / (V * PV).
  }
  execution futures_fixed_base
  report { calendar es_days   periods 252 per year }
}
