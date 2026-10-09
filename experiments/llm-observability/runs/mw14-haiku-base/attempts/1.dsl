# MW14 weekly momentum book: Clenow score, own-row windows, band rebalancing.
strategy mw14 {
  env equities_1w
  resolution @1d
  mode target

  # Spec constants lifted to parameters.
  param mom_n : Count = 18
  param adv_n : Count = 4
  param ma_n : Count = 40
  param gate_n : Count = 52
  param hist : Count = 1000
  param ann : Scalar = 52
  param min_px : Price<USD> = 3 USD/share
  param min_adv : Notional<USD> = 2_000_000 USD
  param enter_rank : Scalar = 7
  param stay_rank : Scalar = 28
  param liq_deciles : Scalar = 10
  param liq_cut : Scalar = 4
  param crowd_hi : Scalar = 1.5
  param crowd_frac : Scalar = 0.25
  param crowd_scale : Scalar = 0.70
  param wcap : Scalar = 0.15
  param dust : Scalar = 0.000001
  param band : Scalar = 0.025
  param tol : Scalar = 1e-12
  param base : Notional<USD> = 1_000_000 USD

  rel week(@T: Timestamp)
  week(T) :- universe(_, T).

  # Split-adjusted close: raw close times the product of split factors over own rows.
  rel lsf(+A: Equity, @T: Timestamp, -L: Scalar)
  lsf(A, T, L) :- universe(A, T), split(A, T, F), L = log(F).
  lsf(A, T, L) :- universe(A, T), not split(A, T, _), L = 0.

  rel cumlog(+A: Equity, @T: Timestamp, -C: Scalar)
  cumlog(A, T, C) :- universe(A, T),
      C = sum(L) over (T1 in rows(T, hist, min 1), universe(A, T1), lsf(A, T1, L)).

  rel adjpx(+A: Equity, @T: Timestamp, -PA: Price<USD>)
  adjpx(A, T, PA) :- universe(A, T), close(A, T, P), cumlog(A, T, C), PA = P * exp(C).

  rel lpx(+A: Equity, @T: Timestamp, -Y: Scalar)
  lpx(A, T, Y) :- adjpx(A, T, PA), Y = log(PA / 1 USD/share).

  # Row number counted over the stock's own rows.
  rel rowno(+A: Equity, @T: Timestamp, -X: Scalar)
  rowno(A, T, X) :- universe(A, T),
      C = count(T1) over (T1 in rows(T, hist, min 1), universe(A, T1)), X = C / 1.

  # Clenow momentum over the last 18 own rows.
  rel score(+A: Equity, @T: Timestamp, -S: Scalar)
  score(A, T, S) :- universe(A, T),
      B = ols_beta(Y, X) over (T1 in rows(T, mom_n, min mom_n), universe(A, T1), lpx(A, T1, Y), rowno(A, T1, X)),
      R = corr(Y, X) over (T1 in rows(T, mom_n, min mom_n), universe(A, T1), lpx(A, T1, Y), rowno(A, T1, X)),
      S = (exp(ann * B) - 1) * R * R.

  # Eligibility (universe).
  rel adv4(+A: Equity, @T: Timestamp, -D: Notional<USD>)
  adv4(A, T, D) :- universe(A, T),
      D = mean(V) over (T1 in rows(T, adv_n, min adv_n), universe(A, T1), dvol(A, T1, V)).

  rel elig(+A: Equity, @T: Timestamp)
  elig(A, T) :- universe(A, T), member(A, T, "SP1500"), close(A, T, P), P >= min_px,
      adv4(A, T, D), D >= min_adv.

  # Score rank among eligible stocks with a score.
  rel escore(+A: Equity, @T: Timestamp, -S: Scalar)
  escore(A, T, S) :- elig(A, T), score(A, T, S).

  rel srank(+A: Equity, @T: Timestamp, -K: Scalar)
  srank(A, T, K) :- week(T), rank(escore(A, T, S), by (S desc, A asc), as K).

  # Liquidity decile among eligible stocks (average ranks on ties).
  rel eadv(+A: Equity, @T: Timestamp, -D: Notional<USD>)
  eadv(A, T, D) :- elig(A, T), adv4(A, T, D).

  rel lrank(+A: Equity, @T: Timestamp, -R: Scalar)
  lrank(A, T, R) :- week(T), rank(eadv(A, T, D), by (D asc), ties average, as R).

  rel neligible(@T: Timestamp, -N: Count)
  neligible(T, N) :- week(T), N = count(A) over (elig(A, T)).

  rel liquid(+A: Equity, @T: Timestamp)
  liquid(A, T) :- lrank(A, T, R), neligible(T, N), Q = R / N, Q * liq_deciles > liq_cut.

  # Market gate and panic.
  rel gate(@T: Timestamp)
  gate(T) :- series(T, "SPXTR", V),
      M = mean(V1) over (T1 in rows(T, gate_n, min gate_n), series(T1, "SPXTR", V1)), V > M.

  rel calm(@T: Timestamp)
  calm(T) :- series(T, "VIX", V), series(T, "VIX_MA4", M4), V <= M4.

  rel panic(@T: Timestamp)
  panic(T) :- week(T), not gate(T), not calm(T).

  # Stay: rank <= 28, or adjusted close above its 40-row mean.
  rel m40(+A: Equity, @T: Timestamp, -M: Price<USD>)
  m40(A, T, M) :- universe(A, T),
      M = mean(PA) over (T1 in rows(T, ma_n, min ma_n), universe(A, T1), adjpx(A, T1, PA)).

  rel above40(+A: Equity, @T: Timestamp)
  above40(A, T) :- adjpx(A, T, PA), m40(A, T, M), PA > M.

  rel may_stay(+A: Equity, @T: Timestamp)
  may_stay(A, T) :- srank(A, T, K), K <= stay_rank.
  may_stay(A, T) :- above40(A, T).

  # Enter: rank <= 7, gate holds, liquid.
  rel enter(+A: Equity, @T: Timestamp)
  enter(A, T) :- srank(A, T, K), K <= enter_rank, gate(T), liquid(A, T).

  # Latch: may stay and (enters now or latched at own previous row), written as
  # "entered in the current run of may-stay own rows".
  rel ind(+A: Equity, @T: Timestamp, -I: Scalar)
  ind(A, T, I) :- universe(A, T), may_stay(A, T), I = 0.
  ind(A, T, I) :- universe(A, T), not may_stay(A, T), I = 1.

  rel exits(+A: Equity, @T: Timestamp, -X: Scalar)
  exits(A, T, X) :- universe(A, T),
      X = sum(I) over (T1 in rows(T, hist, min 1), universe(A, T1), ind(A, T1, I)).

  rel latched(+A: Equity, @T: Timestamp)
  latched(A, T) :- may_stay(A, T), exits(A, T, X),
      C = count(T1) over (T1 in rows(T, hist, min 1), universe(A, T1), enter(A, T1), exits(A, T1, XE), XE = X),
      C > 0.

  # Crowding dial and scale.
  rel nlat(@T: Timestamp, -N: Count)
  nlat(T, N) :- week(T), N = count(A) over (latched(A, T), elig(A, T)).

  rel cpop(@T: Timestamp, -N: Count)
  cpop(T, N) :- week(T), N = count(A) over (latched(A, T), m40(A, T, M)).

  rel cnum(@T: Timestamp, -N: Count)
  cnum(T, N) :- week(T), N = count(A) over (latched(A, T), m40(A, T, M), adjpx(A, T, PA), PA > M * crowd_hi).

  rel crowded(@T: Timestamp)
  crowded(T) :- cpop(T, P), cnum(T, C), C > crowd_frac * P.

  rel scale(@T: Timestamp, -S: Scalar)
  scale(T, S) :- panic(T), S = 0.
  scale(T, S) :- week(T), not panic(T), crowded(T), S = crowd_scale.
  scale(T, S) :- week(T), not panic(T), not crowded(T), S = 1.

  # Target weights for latched, eligible stocks.
  rel tw(+A: Equity, @T: Timestamp, -W: Scalar)
  tw(A, T, W) :- latched(A, T), elig(A, T), nlat(T, N), scale(T, S),
      W = least(S / N, wcap), W > dust.

  # Current book.
  rel held(+A: Equity, @T: Timestamp, -Q: Quantity<Shares>)
  held(A, T, Q) :- position(A, T, Q), Q > 0 shares.

  rel cw(+A: Equity, @T: Timestamp, -C: Scalar)
  cw(A, T, C) :- held(A, T, Q), trclose(A, T, P), C = Q * P / base.

  # Trading band.
  decide(T, target_weight(A, W, moc)) :- tw(A, T, W), not held(A, T, _), W >= band - tol.
  decide(T, target_weight(A, W, moc)) :- tw(A, T, W), cw(A, T, C), D = W - C, abs(D) >= band - tol.
  decide(T, target_weight(A, 0, moc)) :- held(A, T, _), not tw(A, T, _).
}
