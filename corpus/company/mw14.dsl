# MW14: the weekly long-only S&P 1500 momentum book (stageanalyst
# strategy/ivmom/book.py, canon re-frozen 2026-08-14; engine_weighted.py,
# run_mw1_core.py, run_mw3_iv.py, data.py). Data: scripts/ingest/ndlake_weekly.py
# -> env equities_1w (one bar per market week).
#
# Run as the book runs (fixed $1M, trades at the signal close, 10 bps a side):
#   abt run --strategy mw14 --bundle B --price-relation trclose --actions in-prices \
#     --compounding off --capital 1000000 --lot fractional --frictionless --commission-bps 10 \
#     --margin-rate 0 --on-leverage allow --delist-proceeds last-price \
#     --start 1991-01-01 --periods-per-year 52 corpus/env corpus/company/mw14.dsl
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
  env equities_1w
  resolution @1d
  mode target

  param n_enter : Scalar = 7
  param n_stay : Scalar = 28
  param clenow_w : Count = 18
  param ma_w : Count = 40
  param gate_w : Count = 52
  param dvol_w : Count = 4
  param weeks_a_year : Scalar = 52
  param price_floor : Price<USD> = 3 USD/share
  param dvol_floor : Notional<USD> = 2000000 USD
  param one_price : Price<USD> = 1 USD/share
  param deciles : Scalar = 10
  param liq_cut : Scalar = 4
  param crowd_ext : Scalar = 0.5
  param crowd_on : Scalar = 0.25
  param crowd_dial : Scalar = 0.7
  param full : Scalar = 1
  param name_cap : Scalar = 0.15
  param min_weight : Scalar = 0.000001
  param band : Scalar = 0.025
  param band_eps : Scalar = 0.000000000001
  param base : Notional<USD> = 1000000 USD

  rel bar(@T: Timestamp)
  bar(T) :- universe(_, T).

  # A's previous row exists (rows step over a name's own bars, gaps included).
  rel seen_before(+A: Equity, @T: Timestamp)
  seen_before(A, T) :- universe(A, T), prev(T, T1), universe(A, _) asof T1.

  # The cumulative split factor over A's rows, and the split-adjusted close.
  rel split1(+A: Equity, @T: Timestamp, -F: Scalar)
  split1(A, T, F) :- universe(A, T), split(A, T, F).
  split1(A, T, 1) :- universe(A, T), not split(A, T, _).
  rel cumf(+A: Equity, @T: Timestamp, -C: Scalar)
  cumf(A, T, C) :- universe(A, T), not seen_before(A, T), split1(A, T, C).
  cumf(A, T, C) :- seen_before(A, T), prev(T, T1), cumf(A, _, C0) asof T1, split1(A, T, F), C = C0 * F.
  rel adj(+A: Equity, @T: Timestamp, -P: Price<USD>)
  adj(A, T, P) :- close(A, T, P0), cumf(A, T, C), P = P0 * C.

  # A's row number: the regressor of the Clenow fit.
  rel row(+A: Equity, @T: Timestamp, -K: Scalar)
  row(A, T, 0) :- universe(A, T), not seen_before(A, T).
  row(A, T, K) :- seen_before(A, T), prev(T, T1), row(A, _, K0) asof T1, K = K0 + 1.

  # R1: the Clenow score over A's last 18 rows.
  rel clenow(+A: Equity, @T: Timestamp, -S: Scalar)
  clenow(A, T, S) :- universe(A, T),
      B = ols_beta(log(P / one_price), K) over (T1 in rows(T, clenow_w, min clenow_w), adj(A, T1, P), row(A, T1, K)),
      R = corr(log(P / one_price), K) over (T1 in rows(T, clenow_w, min clenow_w), adj(A, T1, P), row(A, T1, K)),
      S = (exp(B * weeks_a_year) - 1) * R * R.

  # The universe.
  rel dvol4(+A: Equity, @T: Timestamp, -D: Notional<USD>)
  dvol4(A, T, D) :- universe(A, T), D = mean(V) over (T1 in rows(T, dvol_w, min dvol_w), dvol(A, T1, V)).
  rel eligible(-A: Equity, @T: Timestamp, -D: Notional<USD>)
  eligible(A, T, D) :- universe(A, T), member(A, T, "SP1500"), close(A, T, P), P >= price_floor, dvol4(A, T, D), D >= dvol_floor.

  rel scored(-A: Equity, @T: Timestamp, -S: Scalar)
  scored(A, T, S) :- eligible(A, T, _), clenow(A, T, S).
  rel xrank(-A: Equity, @T: Timestamp, -K: Scalar)
  xrank(A, T, K) :- bar(T), rank(scored(A, T, S), by (S desc, A asc), as K).

  # The dollar-volume decile: ceil(rank / n x 10) >= 5, i.e. rank / n x 10 > 4.
  rel liqrank(-A: Equity, @T: Timestamp, -K: Scalar)
  liqrank(A, T, K) :- bar(T), rank(eligible(A, T, D), by (D asc), ties average, as K).
  rel n_eligible(@T: Timestamp, -N: Count)
  n_eligible(T, N) :- bar(T), N = count(A) over (eligible(A, T, _)).
  rel liquid(+A: Equity, @T: Timestamp)
  liquid(A, T) :- liqrank(A, T, K), n_eligible(T, N), K / N * deciles > liq_cut.

  # The gate: $SPXTR above its 52-week mean (current week included).
  rel gate(@T: Timestamp)
  gate(T) :- series(T, "SPXTR", V), M = mean(X) over (T1 in rows(T, gate_w, min gate_w), series(T1, "SPXTR", X)), V > M.
  # R5: calm when VIX is at or below its 4-week mean (no reading counts as high).
  rel calm(@T: Timestamp)
  calm(T) :- series(T, "VIX", V), series(T, "VIX_MA4", M), V <= M.
  rel unpanicked(@T: Timestamp)
  unpanicked(T) :- bar(T), gate(T).
  unpanicked(T) :- bar(T), calm(T).

  # R2, R3 and the latch, carried as 1 or 0 at every row of A.
  rel ma(+A: Equity, @T: Timestamp, -M: Price<USD>)
  ma(A, T, M) :- universe(A, T), M = mean(P) over (T1 in rows(T, ma_w, min ma_w), adj(A, T1, P)).
  rel enter(+A: Equity, @T: Timestamp)
  enter(A, T) :- xrank(A, T, K), K <= n_enter, gate(T), liquid(A, T).
  rel stay(+A: Equity, @T: Timestamp)
  stay(A, T) :- xrank(A, T, K), K <= n_stay.
  stay(A, T) :- adj(A, T, P), ma(A, T, M), P > M.
  rel lp(+A: Equity, @T: Timestamp, -L: Scalar)
  lp(A, T, 1) :- universe(A, T), enter(A, T).
  lp(A, T, L) :- universe(A, T), not enter(A, T), stay(A, T), seen_before(A, T), prev(T, T1), lp(A, _, L) asof T1.
  lp(A, T, 0) :- universe(A, T), not enter(A, T), stay(A, T), not seen_before(A, T).
  lp(A, T, 0) :- universe(A, T), not enter(A, T), not stay(A, T).

  # R7: the crowding dial over latched names (in the universe or not).
  rel ext(+A: Equity, @T: Timestamp, -E: Scalar)
  ext(A, T, E) :- adj(A, T, P), ma(A, T, M), E = P / M - 1.
  rel n_latched(@T: Timestamp, -N: Count)
  n_latched(T, N) :- bar(T), N = count(A) over (universe(A, T), lp(A, T, 1), ext(A, T, _)).
  rel n_crowded(@T: Timestamp, -N: Count)
  n_crowded(T, N) :- bar(T), N = count(A) over (universe(A, T), lp(A, T, 1), ext(A, T, E), E > crowd_ext).
  rel crowded(@T: Timestamp)
  crowded(T) :- n_latched(T, N), N > 0, n_crowded(T, C), C / N > crowd_on.
  rel scale(@T: Timestamp, -S: Scalar)
  scale(T, crowd_dial) :- unpanicked(T), crowded(T).
  scale(T, full) :- unpanicked(T), not crowded(T).

  # R4: weights over the latched eligible names.
  rel inbook(-A: Equity, @T: Timestamp)
  inbook(A, T) :- eligible(A, T, _), lp(A, T, 1).
  rel n_book(@T: Timestamp, -N: Count)
  n_book(T, N) :- bar(T), N = count(A) over (inbook(A, T)).
  rel w(+A: Equity, @T: Timestamp, -W: Scalar)
  w(A, T, W) :- inbook(A, T), n_book(T, N), scale(T, S), W = least(1 / N * S, name_cap), W > min_weight.
  rel tgt(-A: Equity, @T: Timestamp, -W: Scalar)
  tgt(A, T, W) :- inbook(A, T), w(A, T, W).

  # R6: the book's weight on the fixed base, and the band.
  rel held(-A: Equity, @T: Timestamp, -Q: Quantity<Shares>)
  held(A, T, Q) :- position(A, T, Q), Q > 0 shares.
  rel cur(+A: Equity, @T: Timestamp, -C: Scalar)
  cur(A, T, C) :- held(A, T, Q), trclose(A, T, P), C = Q * P / base.

  decide(T, target_weight(A, W, moc)) :- tgt(A, T, W), not held(A, T, _), W >= band - band_eps.
  decide(T, target_weight(A, W, moc)) :- tgt(A, T, W), cur(A, T, C), W - C >= band - band_eps.
  decide(T, target_weight(A, W, moc)) :- tgt(A, T, W), cur(A, T, C), C - W >= band - band_eps.
  decide(T, target_weight(A, 0, moc)) :- held(A, T, _), not tgt(A, T, _).
}
