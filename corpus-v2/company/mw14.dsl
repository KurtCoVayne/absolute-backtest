# MW14: the weekly long-only S&P 1500 momentum book (stageanalyst
# strategy/ivmom/book.py, canon re-frozen 2026-08-14; engine_weighted.py,
# run_mw1_core.py, run_mw3_iv.py, data.py). Rules R1 to R7 are the book's
# numbering; `docs/language-v2.md` section 9.1 reads this file against them.
# The run is the program: the execution and report blocks replace the
# sixteen flags of the first formulation's command line, and the weekly
# facts are the daily facts bucketed, so the demand is daily bars.
#
# Rules (book.py docstring numbering):
#   universe  member of the S&P 500/400/600 at the week end, raw close >= $3,
#             4-week mean of weekly mean daily dollar volume >= $2M
#   R1 rank   Clenow: (exp(52 b) - 1) R^2 of the 18-row OLS of log adjusted close
#             on time; ordinal rank, best first, ties by asset id; no score, no rank
#   R2 enter  rank <= 7, $SPXTR above its 52-week mean, dollar-volume decile >= 5
#   R3 stay   rank <= 28, or adjusted close above its 40-row mean
#   latch     in book = stay and (enter or in book at the name's previous row)
#   R5 panic  scale 0 unless the gate holds or VIX is at or below its 4-week mean
#   R7 dial   scale x 0.70 when more than 25% of latched names (with a 40-row
#             mean) trade more than 50% above it
#   R4 weight min(scale / N, 0.15) over the N latched eligible names
#   R6 trade  at the week's close; a name's change fires when it is at least the
#             2.5% band (1e-12 tolerance); exits always fire
#
# Assumptions and known differences from the Python book, each small:
#   - A held name with no bar in a week (a halt) is kept and marked at its last
#     price; the book drops it at no cost and may re-buy it. 74 member
#     name-weeks in 1990-2026 lack a bar.
#   - A held name's weight drifts as quantity x price / base here and as
#     w (1 + r) there: equal up to rounding, which can flip a trade at the edge
#     of the band.
#   - Decisions on the data's last week have no next bar to settle at; the book
#     books that week's cost with no return.
strategy mw14 {
  data {
    class    equity
    need     bars @1d from 1990-01-01 complete      # a reviewed store: a missing bar means the name did not trade
    need     actions, listing
    need     lists "SP500", "SP400", "SP600", "ETF"
    need     series "SPXTR" @1d, "VIX" @1w complete
    require  reviewed
    prefer   latest
  }
  uses      calendar, catalog, bars, state, book
  calendars { weekly = weeks(1) }
  clock     { decide on weekly }

  universe {
    # A name's rows are the weeks it traded; the ETFs the bundle carries (SPY, QQQ) are not in the book.
    universe(A, T) :- traded@weekly(A, T), not member(A, T, "ETF").
  }

  params {
    n_enter      : Count = 7
    n_stay       : Count = 28
    clenow_rows  : Count = 18
    ma_rows      : Count = 40
    gate_rows    : Count = 52
    dvol_rows    : Count = 4
    vix_rows     : Count = 4
    rows_a_year  : Scalar = 52
    price_floor  : Price<USD> = 3 USD/share
    dvol_floor   : Notional<USD> = 2_000_000 USD
    liq_decile   : Count = 5
    crowd_ext    : Scalar = 0.5
    crowd_share  : Scalar = 0.25
    crowd_dial   : Scalar = 0.7
    name_cap     : Scalar = 0.15
    min_weight   : Scalar = 1e-6
    band         : Scalar = 0.025
    band_tol     : Scalar = 1e-12
  }

  rules {
    # Weekly inputs, as views of the daily bars: the week's mean daily dollar volume.
    rel dvol(A: Equity, @T, D: Notional<USD>)
    dvol(A, T, D) :- universe(A, T), D = mean(X) over (T1 in bucket(weekly, T), dollar_volume(A, T1, X)).

    # R1: the Clenow score over the name's last 18 rows (a price ratio is a Scalar, F-9).
    rel clenow(A: Equity, @T, S: Scalar)
    clenow(A, T, S) :- universe(A, T), P0 = 1 USD/share,
        B = ols_slope(log(P / P0), K) over (T1 in rows[clenow_rows], adj_close@weekly(A, T1, P), row_number(A, T1, K)),
        R = ols_r(log(P / P0), K)     over (T1 in rows[clenow_rows], adj_close@weekly(A, T1, P), row_number(A, T1, K)),
        S = (exp(B * rows_a_year) - 1) * R * R.

    # The S&P 1500 is the union of the three indices.
    rel sp1500(A: Equity, @T)
    sp1500(A, T) :- member(A, T, "SP500").
    sp1500(A, T) :- member(A, T, "SP400").
    sp1500(A, T) :- member(A, T, "SP600").

    # The universe of the book: members above the price floor and the dollar-volume floor.
    rel eligible(A: Equity, @T, D: Notional<USD>)
    eligible(A, T, D) :- universe(A, T), sp1500(A, T),
        close@weekly(A, T, P), P >= price_floor,
        D = mean(X) over (T1 in rows[dvol_rows], dvol(A, T1, X)), D >= dvol_floor.

    rel scored(A: Equity, @T, S: Scalar)
    scored(A, T, S) :- eligible(A, T, _), clenow(A, T, S).
    rel ranked(A: Equity, @T, K: Count)
    ranked(A, T, K) :- K = rank(scored(A, T, S) by (S desc, A asc)) within (T).

    # The dollar-volume decile, ties averaged: ceil(rank / n x 10) >= 5.
    rel liquid(A: Equity, @T)
    liquid(A, T) :- K = rank_average(eligible(A, T, D) by (D asc)) within (T),
        N = count(A1) over (eligible(A1, T, _)) within (T),
        decile(K, N) >= liq_decile.

    # The gate and the calm test on the market series (the daily SPXTR is read as its weekly bar view).
    rel gate(@T)
    gate(T) :- value("SPXTR", T, V), M = mean(X) over (T1 in rows[gate_rows], value("SPXTR", T1, X)), V > M.
    rel calm(@T)
    calm(T) :- value("VIX", T, V), M = mean(X) over (T1 in rows[vix_rows], value("VIX", T1, X)), V <= M.

    # R2, R3 and the latch.
    rel ma(A: Equity, @T, M: Price<USD>)
    ma(A, T, M) :- universe(A, T), M = mean(P) over (T1 in rows[ma_rows], adj_close@weekly(A, T1, P)).
    rel enter(A: Equity, @T)
    enter(A, T) :- ranked(A, T, K), K <= n_enter, gate(T), liquid(A, T).
    rel stay(A: Equity, @T)
    stay(A, T) :- ranked(A, T, K), K <= n_stay.
    stay(A, T) :- adj_close@weekly(A, T, P), ma(A, T, M), P > M.
    rel latched(A: Equity, @T)
    latched(A, T) :- latch[enter, stay](A, T).

    # R5 and R7: the scale, one case form.
    rel extension(A: Equity, @T, E: Scalar)
    extension(A, T, E) :- adj_close@weekly(A, T, P), ma(A, T, M), E = P / M - 1.
    rel crowded(@T)
    crowded(T) :- bar(T),
        N = count(A) over (latched(A, T), extension(A, T, _)) within (T), N > 0,
        C = count(A) over (latched(A, T), extension(A, T, E), E > crowd_ext) within (T),
        C / N > crowd_share.
    rel scale(@T, S: Scalar)
    scale(T, S) :- bar(T), S = case { not gate(T), not calm(T) -> 0 ;
                                     crowded(T)                -> crowd_dial ;
                                     else                      -> 1 }.

    # R4: equal weights over the latched eligible names, capped.
    rel in_book(A: Equity, @T)
    in_book(A, T) :- eligible(A, T, _), latched(A, T).
    rel target(A: Equity, @T, W: Scalar)
    target(A, T, W) :- in_book(A, T), N = count(A1) over (in_book(A1, T)) within (T),
        scale(T, S), W = least(S / N, name_cap), W > min_weight.

    # Observed for the researcher (8.1): the scale at every week and the latch
    # with the literal that excluded each name, in the ledger, without
    # changing a decision (Prop. 8.4).
    observe scale
    observe latched trace instances
  }

  decisions {
    silence hold
    orders  moc
    # R6: trade at the week's close when the change clears the band; exits always.
    decide(T, hold(A, W)) :- target(A, T, W), weight_now(A, T, C), abs(W - C) >= band - band_tol.
    decide(T, hold(A, W)) :- target(A, T, W), not held(A, T, _), W >= band - band_tol.
    decide(T, hold(A, 0)) :- held(A, T, _), not target(A, T, _).
  }

  # Fixed $1M, trades at the signal close at the total-return price, 10 bps a side.
  execution book_fixed_base

  report {
    calendar weekly
    periods  52 per year
    window   1991-01-01 .. 2026-07-02
  }
}
