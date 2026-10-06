strategy mw14 {
  env equities_1w
  resolution @1d
  mode target

  param band : Scalar = 0.025
  param tol : Scalar = 1e-12
  param nmom : Count = 18
  param nliq : Count = 4
  param nma : Count = 40
  param ngate : Count = 52
  param minpx : Price<USD> = 3 USD/share
  param mindv : Notional<USD> = 2_000_000 USD
  param ann : Scalar = 52
  param enter_rank : Scalar = 7
  param stay_rank : Scalar = 28
  param deciles : Scalar = 10
  param liq_cut : Scalar = 4
  param crowd_mult : Scalar = 1.5
  param crowd_frac : Scalar = 0.25
  param crowd_scale : Scalar = 0.7
  param cap : Scalar = 0.15
  param minw : Scalar = 0.000001
  param base : Notional<USD> = 1_000_000 USD

  rel bar(@T: Timestamp)
  bar(T) :- universe(_, T).

  # stock's own rows
  rel has_prev(+A: Equity, @T: Timestamp)
  has_prev(A, T) :- universe(A, T), prev(T, Tp), close(A, _, _) asof Tp.

  rel split_or_one(+A: Equity, @T: Timestamp, -F: Scalar)
  split_or_one(A, T, F) :- universe(A, T), split(A, T, F).
  split_or_one(A, T, 1) :- universe(A, T), not split(A, T, _).

  rel cum(+A: Equity, @T: Timestamp, -C: Scalar)
  cum(A, T, C) :- universe(A, T), not has_prev(A, T), split_or_one(A, T, C).
  cum(A, T, C) :- universe(A, T), prev(T, Tp), close(A, T0, _) asof Tp,
      cum(A, T0, C0), split_or_one(A, T, F), C = C0 * F.

  rel adj(+A: Equity, @T: Timestamp, -P: Price<USD>)
  adj(A, T, P) :- close(A, T, P0), cum(A, T, C), P = P0 * C.

  rel lc(+A: Equity, @T: Timestamp, -Y: Scalar)
  lc(A, T, Y) :- adj(A, T, P), Y = log(P / 1 USD/share).

  rel rown(+A: Equity, @T: Timestamp, -X: Scalar)
  rown(A, T, 0) :- universe(A, T), not has_prev(A, T).
  rown(A, T, X) :- universe(A, T), prev(T, Tp), close(A, T0, _) asof Tp,
      rown(A, T0, X0), X = X0 + 1.

  rel ma40(+A: Equity, @T: Timestamp, -M: Price<USD>)
  ma40(A, T, M) :- universe(A, T),
      M = mean(P) over (T1 in rows(T, nma, min nma), adj(A, T1, P)).

  rel dv4(+A: Equity, @T: Timestamp, -D: Notional<USD>)
  dv4(A, T, D) :- universe(A, T),
      D = mean(V) over (T1 in rows(T, nliq, min nliq), dvol(A, T1, V)).

  rel eligible(-A: Equity, @T: Timestamp)
  eligible(A, T) :- universe(A, T), member(A, T, "SP1500"), close(A, T, P),
      P >= minpx, dv4(A, T, D), D >= mindv.

  rel score(+A: Equity, @T: Timestamp, -S: Scalar)
  score(A, T, S) :- universe(A, T),
      B = ols_beta(Y, X) over (T1 in rows(T, nmom, min nmom), lc(A, T1, Y), rown(A, T1, X)),
      R = corr(Y, X) over (T1 in rows(T, nmom, min nmom), lc(A, T1, Y), rown(A, T1, X)),
      S = (exp(ann * B) - 1) * R * R.

  rel cand(-A: Equity, @T: Timestamp, -S: Scalar)
  cand(A, T, S) :- eligible(A, T), score(A, T, S).

  rel ranked(-A: Equity, @T: Timestamp, -K: Scalar)
  ranked(A, T, K) :- bar(T), rank(cand(A, T, S), by (S desc, A asc), as K).

  rel elig4(-A: Equity, @T: Timestamp, -D: Notional<USD>)
  elig4(A, T, D) :- eligible(A, T), dv4(A, T, D).

  rel liqrank(-A: Equity, @T: Timestamp, -K: Scalar)
  liqrank(A, T, K) :- bar(T), rank(elig4(A, T, D), by (D asc), ties average, as K).

  rel neligible(@T: Timestamp, -N: Count)
  neligible(T, N) :- bar(T), N = count(A) over (eligible(A, T)).

  rel liquid(+A: Equity, @T: Timestamp)
  liquid(A, T) :- liqrank(A, T, K), neligible(T, N), K / N * deciles > liq_cut.

  rel gate(@T: Timestamp)
  gate(T) :- series(T, "SPXTR", V),
      M = mean(V1) over (T1 in rows(T, ngate, min ngate), series(T1, "SPXTR", V1)), V > M.

  rel calm(@T: Timestamp)
  calm(T) :- series(T, "VIX", V), series(T, "VIX_MA4", M), V <= M.

  rel nopanic(@T: Timestamp)
  nopanic(T) :- gate(T).
  nopanic(T) :- calm(T).

  rel enters(-A: Equity, @T: Timestamp)
  enters(A, T) :- ranked(A, T, K), K <= enter_rank, gate(T), liquid(A, T).

  rel stay(-A: Equity, @T: Timestamp)
  stay(A, T) :- ranked(A, T, K), K <= stay_rank.
  stay(A, T) :- universe(A, T), adj(A, T, P), ma40(A, T, M), P > M.

  rel latched(-A: Equity, @T: Timestamp)
  latched(A, T) :- enters(A, T), stay(A, T).
  latched(A, T) :- stay(A, T), prev(T, Tp), close(A, T0, _) asof Tp, latched(A, T0).

  rel nlat(@T: Timestamp, -L: Count)
  nlat(T, L) :- bar(T), L = count(A) over (latched(A, T), ma40(A, T, _)).
  rel ncrowd(@T: Timestamp, -C: Count)
  ncrowd(T, C) :- bar(T),
      C = count(A) over (latched(A, T), ma40(A, T, M), adj(A, T, P), P > crowd_mult * M).
  rel crowded(@T: Timestamp)
  crowded(T) :- nlat(T, L), L > 0, ncrowd(T, C), C / L > crowd_frac.

  rel scale(@T: Timestamp, -S: Scalar)
  scale(T, 0) :- bar(T), not nopanic(T).
  scale(T, crowd_scale) :- nopanic(T), crowded(T).
  scale(T, 1) :- nopanic(T), not crowded(T).

  rel sel(-A: Equity, @T: Timestamp)
  sel(A, T) :- latched(A, T), eligible(A, T).
  rel nsel(@T: Timestamp, -N: Count)
  nsel(T, N) :- bar(T), N = count(A) over (sel(A, T)).

  rel tgt(-A: Equity, @T: Timestamp, -W: Scalar)
  tgt(A, T, W) :- sel(A, T), nsel(T, N), scale(T, S), W = least(S / N, cap), W > minw.

  rel held(-A: Equity, @T: Timestamp, -Q: Quantity<Shares>)
  held(A, T, Q) :- universe(A, T), position(A, T, Q), Q > 0 shares.

  rel curw(+A: Equity, @T: Timestamp, -W: Scalar)
  curw(A, T, W) :- held(A, T, Q), trclose(A, T, P), W = Q * P / base.

  decide(T, target_weight(A, W, moc)) :- tgt(A, T, W), curw(A, T, C), abs(W - C) >= band - tol.
  decide(T, target_weight(A, W, moc)) :- tgt(A, T, W), not held(A, T, _), W >= band - tol.
  decide(T, target_weight(A, 0, moc)) :- held(A, T, _), not tgt(A, T, _).
}
