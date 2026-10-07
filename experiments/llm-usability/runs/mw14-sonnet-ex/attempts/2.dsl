strategy mw14 {
  env equities_1w
  resolution @1d
  mode target

  param idx : Label = "SP1500"
  param spx : Label = "SPXTR"
  param vix : Label = "VIX"
  param vixma : Label = "VIX_MA4"
  param min_price : Price<USD> = 3 USD/share
  param min_dvol : Notional<USD> = 2_000_000 USD
  param unit : Price<USD> = 1 USD/share
  param adv_rows : Count = 4
  param mom_rows : Count = 18
  param trend_rows : Count = 40
  param gate_rows : Count = 52
  param ann : Scalar = 52
  param enter_rank : Scalar = 7
  param stay_rank : Scalar = 28
  param liq_cut : Scalar = 4
  param deciles : Scalar = 10
  param crowd_mult : Scalar = 1.5
  param crowd_frac : Scalar = 0.25
  param crowd_scale : Scalar = 0.7
  param cap : Scalar = 0.15
  param min_w : Scalar = 0.000001
  param band : Scalar = 0.025
  param tol : Scalar = 1e-12
  param base : Notional<USD> = 1_000_000 USD

  rel bar(@T: Timestamp)
  bar(T) :- universe(_, T).

  # a stock's own previous row exists
  rel hasprior(+A: Equity, @T: Timestamp)
  hasprior(A, T) :- universe(A, T), prev(T, Tp), universe(A, _) asof Tp.

  rel lsp(+A: Equity, @T: Timestamp, -L: Scalar)
  lsp(A, T, L) :- universe(A, T), split(A, T, F), L = log(F).
  lsp(A, T, 0) :- universe(A, T), not split(A, T, _).

  # cumulative log split factor over the stock's own rows
  rel cum(+A: Equity, @T: Timestamp, -C: Scalar)
  cum(A, T, C) :- lsp(A, T, C), not hasprior(A, T).
  cum(A, T, C) :- lsp(A, T, L), hasprior(A, T), prev(T, Tp), cum(A, _, C0) asof Tp, C = C0 + L.

  # row number of the stock's own rows
  rel rownum(+A: Equity, @T: Timestamp, -X: Scalar)
  rownum(A, T, 0) :- universe(A, T), not hasprior(A, T).
  rownum(A, T, X) :- hasprior(A, T), prev(T, Tp), rownum(A, _, X0) asof Tp, X = X0 + 1.

  rel adjc(+A: Equity, @T: Timestamp, -P: Price<USD>)
  adjc(A, T, P) :- close(A, T, P0), cum(A, T, C), P = P0 * exp(C).

  rel ladj(+A: Equity, @T: Timestamp, -Y: Scalar)
  ladj(A, T, Y) :- adjc(A, T, P), Y = log(P / unit).

  rel adv(+A: Equity, @T: Timestamp, -D: Notional<USD>)
  adv(A, T, D) :- universe(A, T),
      D = mean(V) over (T1 in rows(T, adv_rows, min adv_rows), dvol(A, T1, V)).

  rel eligible(-A: Equity, @T: Timestamp)
  eligible(A, T) :- universe(A, T), member(A, T, idx), close(A, T, P), P >= min_price,
      adv(A, T, D), D >= min_dvol.

  rel score(+A: Equity, @T: Timestamp, -S: Scalar)
  score(A, T, S) :- universe(A, T),
      B = ols_beta(Y, X) over (T1 in rows(T, mom_rows, min mom_rows), ladj(A, T1, Y), rownum(A, T1, X)),
      R = corr(Y, X) over (T1 in rows(T, mom_rows, min mom_rows), ladj(A, T1, Y), rownum(A, T1, X)),
      S = (exp(ann * B) - 1) * R * R.

  rel scored(-A: Equity, @T: Timestamp, -S: Scalar)
  scored(A, T, S) :- eligible(A, T), score(A, T, S).

  rel ranked(-A: Equity, @T: Timestamp, -K: Scalar)
  ranked(A, T, K) :- bar(T), rank(scored(A, T, S), by (S desc, A asc), as K).

  rel eligadv(-A: Equity, @T: Timestamp, -D: Notional<USD>)
  eligadv(A, T, D) :- eligible(A, T), adv(A, T, D).

  rel liqrank(-A: Equity, @T: Timestamp, -K: Scalar)
  liqrank(A, T, K) :- bar(T), rank(eligadv(A, T, D), by (D asc), ties average, as K).

  rel n_elig(@T: Timestamp, -N: Count)
  n_elig(T, N) :- bar(T), N = count(A) over (eligible(A, T)).

  rel liquid(-A: Equity, @T: Timestamp)
  liquid(A, T) :- liqrank(A, T, K), n_elig(T, N), K * (1 / N) * deciles > liq_cut.

  rel gate_ma(@T: Timestamp, -M: Scalar)
  gate_ma(T, M) :- bar(T),
      M = mean(V) over (T1 in rows(T, gate_rows, min gate_rows), series(T1, spx, V)).
  rel gate(@T: Timestamp)
  gate(T) :- series(T, spx, V), gate_ma(T, M), V > M.

  rel calm(@T: Timestamp)
  calm(T) :- series(T, vix, V), series(T, vixma, M), V <= M.
  rel panic(@T: Timestamp)
  panic(T) :- bar(T), not gate(T), not calm(T).

  rel enter(-A: Equity, @T: Timestamp)
  enter(A, T) :- ranked(A, T, K), K <= enter_rank, gate(T), liquid(A, T).

  rel trend_ma(+A: Equity, @T: Timestamp, -M: Price<USD>)
  trend_ma(A, T, M) :- universe(A, T),
      M = mean(P) over (T1 in rows(T, trend_rows, min trend_rows), adjc(A, T1, P)).

  rel stay(-A: Equity, @T: Timestamp)
  stay(A, T) :- ranked(A, T, K), K <= stay_rank.
  stay(A, T) :- universe(A, T), adjc(A, T, P), trend_ma(A, T, M), P > M.

  # latch state: 1 latched, 0 not, on every row
  rel lstate(+A: Equity, @T: Timestamp, -S: Scalar)
  lstate(A, T, 1) :- enter(A, T).
  lstate(A, T, 1) :- stay(A, T), not enter(A, T), hasprior(A, T), prev(T, Tp), lstate(A, _, 1) asof Tp.
  lstate(A, T, 0) :- universe(A, T), not stay(A, T).
  lstate(A, T, 0) :- stay(A, T), not enter(A, T), not hasprior(A, T).
  lstate(A, T, 0) :- stay(A, T), not enter(A, T), hasprior(A, T), prev(T, Tp), lstate(A, _, 0) asof Tp.

  rel latched(-A: Equity, @T: Timestamp)
  latched(A, T) :- universe(A, T), lstate(A, T, 1).

  rel crowd_n(@T: Timestamp, -N: Count)
  crowd_n(T, N) :- bar(T), N = count(A) over (latched(A, T), trend_ma(A, T, M)).
  rel crowd_hot(@T: Timestamp, -N: Count)
  crowd_hot(T, N) :- bar(T), N = count(A) over (latched(A, T), trend_ma(A, T, M), adjc(A, T, P), P > crowd_mult * M).
  rel crowded(@T: Timestamp)
  crowded(T) :- crowd_n(T, N), crowd_hot(T, H), H / N > crowd_frac.

  rel scale(@T: Timestamp, -S: Scalar)
  scale(T, crowd_scale) :- crowded(T).
  scale(T, 1) :- bar(T), not crowded(T).

  rel members(-A: Equity, @T: Timestamp)
  members(A, T) :- latched(A, T), eligible(A, T).
  rel n_members(@T: Timestamp, -N: Count)
  n_members(T, N) :- bar(T), N = count(A) over (members(A, T)).

  rel tgt(-A: Equity, @T: Timestamp, -W: Scalar)
  target(A, T, W) :- members(A, T), n_members(T, N), scale(T, S), not panic(T),
      W = least(S * (1 / N), cap), W > min_w.

  rel held(-A: Equity, @T: Timestamp, -Q: Quantity<Shares>)
  held(A, T, Q) :- position(A, T, Q), Q > 0 shares.

  decide(T, target_weight(A, W, moc)) :- target(A, T, W), held(A, T, Q), trclose(A, T, P),
      D = W - Q * P / base, abs(D) >= band - tol.
  decide(T, target_weight(A, W, moc)) :- target(A, T, W), universe(A, T), not held(A, T, _), W >= band.
  decide(T, target_weight(A, 0, moc)) :- held(A, T, _), not tgt(A, T, _).
}
