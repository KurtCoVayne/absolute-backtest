# MW14 weekly momentum book (TASK.md) as strategy mw14 over equities_1w.
# Rule numbers in comments follow the spec's Rules section.
strategy mw14 {
  env equities_1w
  resolution @1d
  mode target

  # The book's constants, lifted into parameters (no ranges: the book fixes them).
  param base : Notional<USD> = 1000000 USD
  param px_min : Price<USD> = 3 USD/share
  param dvol_min : Notional<USD> = 2000000 USD
  param hist : Duration = 100y
  param n_adv : Count = 4
  param n_score : Count = 18
  param n_mean : Count = 40
  param n_gate : Count = 52
  param ann : Scalar = 52
  param rank_enter : Scalar = 7
  param rank_stay : Scalar = 28
  param deciles : Scalar = 10
  param liq_cut : Scalar = 4
  param crowd_mult : Scalar = 1.5
  param crowd_frac : Scalar = 0.25
  param crowd_scale : Scalar = 0.7
  param cap : Scalar = 0.15
  param floor_w : Scalar = 1e-6
  param band : Scalar = 0.025
  param tol : Scalar = 1e-12

  # One row per week that has any stock in the universe.
  rel wk(@T: Timestamp)
  wk(T) :- universe(_, T).

  # Split factor of each of the stock's own rows (1 when there is no split that week).
  rel sfac(+A: Equity, @T: Timestamp, -F: Scalar)
  sfac(A, T, F) :- universe(A, T), split(A, T, F).
  sfac(A, T, 1) :- universe(A, T), not split(A, T, _).

  # Log of the cumulative split factor over the stock's own rows up to and including T.
  rel cumlf(+A: Equity, @T: Timestamp, -L: Scalar)
  cumlf(A, T, L) :- universe(A, T),
      L = sum(LF) over (T1 in window(T, hist, min 1), sfac(A, T1, F), LF = log(F)).

  # Split-adjusted close: raw close times the cumulative split factor.
  rel padj(+A: Equity, @T: Timestamp, -P: Price<USD>)
  padj(A, T, P) :- universe(A, T), close(A, T, C), cumlf(A, T, L), P = C * exp(L).

  # Natural log of the split-adjusted close: the regression's y.
  rel lnpx(+A: Equity, @T: Timestamp, -Y: Scalar)
  lnpx(A, T, Y) :- padj(A, T, P), Y = log(P / 1 USD/share).

  # The stock's row number since its first row (0, 1, 2, ...): the regression's x.
  rel rowno(+A: Equity, @T: Timestamp, -X: Scalar)
  rowno(A, T, X) :- universe(A, T),
      S = sum(1.0) over (T1 in window(T, hist, min 1), universe(A, T1)), X = S - 1.0.

  # Clenow score over the stock's last n_score rows (rule 2).
  rel score(+A: Equity, @T: Timestamp, -S: Scalar)
  score(A, T, S) :- universe(A, T),
      B = ols_beta(Y, X) over (T1 in rows(T, n_score, min n_score), universe(A, T1), lnpx(A, T1, Y), rowno(A, T1, X)),
      Rho = corr(Y, X) over (T1 in rows(T, n_score, min n_score), universe(A, T1), lnpx(A, T1, Y), rowno(A, T1, X)),
      S = (exp(ann * B) - 1) * Rho * Rho.

  # Mean weekly dollar volume over the stock's last n_adv rows.
  rel adv4(+A: Equity, @T: Timestamp, -D: Notional<USD>)
  adv4(A, T, D) :- universe(A, T),
      D = mean(V) over (T1 in rows(T, n_adv, min n_adv), universe(A, T1), dvol(A, T1, V)).

  # Mean split-adjusted close over the stock's last n_mean rows.
  rel m40(+A: Equity, @T: Timestamp, -M: Price<USD>)
  m40(A, T, M) :- universe(A, T),
      M = mean(P) over (T1 in rows(T, n_mean, min n_mean), universe(A, T1), padj(A, T1, P)).

  # Eligible (rule 1).
  rel eligible(-A: Equity, @T: Timestamp)
  eligible(A, T) :- universe(A, T), member(A, T, "SP1500"), close(A, T, P), P >= px_min,
      adv4(A, T, D), D >= dvol_min.

  rel eligadv(-A: Equity, @T: Timestamp, -D: Notional<USD>)
  eligadv(A, T, D) :- eligible(A, T), adv4(A, T, D).

  # Liquidity rank among eligible stocks, ascending, ties sharing the average rank (rule 5).
  rel liqrank(-A: Equity, @T: Timestamp, -LR: Scalar)
  liqrank(A, T, LR) :- wk(T), rank(eligadv(A, T, D), by (D asc), ties average, as LR).

  # Score rank among eligible stocks that have a score (rule 3).
  rel scored(-A: Equity, @T: Timestamp, -S: Scalar)
  scored(A, T, S) :- eligible(A, T), score(A, T, S).

  rel scrank(-A: Equity, @T: Timestamp, -RS: Scalar)
  scrank(A, T, RS) :- wk(T), rank(scored(A, T, S), by (S desc, A asc), as RS).

  # Gate (rule 4): SPXTR above its mean over the last n_gate weeks, this week included.
  rel gate(@T: Timestamp)
  gate(T) :- series(T, "SPXTR", V),
      M = mean(V1) over (T1 in rows(T, n_gate, min n_gate), series(T1, "SPXTR", V1)), V > M.

  # VIX at or below its 4-week mean (rule 9).
  rel vixcalm(@T: Timestamp)
  vixcalm(T) :- series(T, "VIX", V), series(T, "VIX_MA4", M), V <= M.

  rel calm(@T: Timestamp)
  calm(T) :- gate(T).
  calm(T) :- vixcalm(T).

  # Panic (rule 9): neither the gate nor a calm VIX.
  rel panic(@T: Timestamp)
  panic(T) :- wk(T), not calm(T).

  # Liquid (rule 5): rank / n x 10 > 4.
  rel liquid(-A: Equity, @T: Timestamp)
  liquid(A, T) :- liqrank(A, T, LR), NE = count(A1) over (eligible(A1, T)),
      LR / NE * deciles > liq_cut.

  # Stay (rule 7) and enter (rule 6).
  rel may_stay(-A: Equity, @T: Timestamp)
  may_stay(A, T) :- scrank(A, T, RS), RS <= rank_stay.
  may_stay(A, T) :- universe(A, T), padj(A, T, P), m40(A, T, M), P > M.

  rel enter(-A: Equity, @T: Timestamp)
  enter(A, T) :- scrank(A, T, RS), RS <= rank_enter, gate(T), liquid(A, T).

  # Latch (rule 8): may stay, and enters or was latched at its own previous row.
  rel latched(-A: Equity, @T: Timestamp)
  latched(A, T) :- enter(A, T), may_stay(A, T).
  latched(A, T) :- may_stay(A, T), prev(T, T1), universe(A, T0) asof T1, latched(A, T0).

  # Crowding (rule 10): more than 25% of latched stocks with a 40-row mean sit 50% above it.
  rel crowded(@T: Timestamp)
  crowded(T) :- wk(T),
      NL = count(A1) over (latched(A1, T), m40(A1, T, M1)), NL >= 1,
      NC = count(A2) over (latched(A2, T), m40(A2, T, M2), padj(A2, T, P2), P2 > crowd_mult * M2),
      NC / NL > crowd_frac.

  # Scale (rules 9 and 10).
  rel dial(@T: Timestamp, -S: Scalar)
  dial(T, 0) :- panic(T).
  dial(T, crowd_scale) :- wk(T), not panic(T), crowded(T).
  dial(T, 1) :- wk(T), not panic(T), not crowded(T).

  # Target weights (rule 11): latched and eligible names share the scale, capped.
  rel tgt(-A: Equity, @T: Timestamp, -W: Scalar)
  tgt(A, T, W) :- latched(A, T), eligible(A, T), dial(T, S),
      N = count(A1) over (latched(A1, T), eligible(A1, T)),
      W = least(S / N, cap), W > floor_w.

  # Held positions, from the executor (feeds the trading band, rule 12).
  rel held(-A: Equity, @T: Timestamp, -Q: Quantity<Shares>)
  held(A, T, Q) :- position(A, T, Q), Q > 0 shares.

  # Trading band (rule 12). Held names move when the change is at least 2.5% of base;
  # new names are bought when the target is at least 2.5%; a held name with no target is sold.
  decide(T, target_weight(A, W, moc)) :- tgt(A, T, W), held(A, T, Q), trclose(A, T, P),
      abs(W - Q * P / base) >= band - tol.
  decide(T, target_weight(A, W, moc)) :- tgt(A, T, W), not held(A, T, _), W >= band - tol.
  decide(T, target_weight(A, 0, moc)) :- held(A, T, _), not tgt(A, T, _).

  # ---- diagnostics for the audit (attempt 2 only; nothing here feeds decide) ----
  rel dg_cnt(@T: Timestamp, -NL: Count, -NC: Count)
  dg_cnt(T, NL, NC) :- wk(T),
      NL = count(A1) over (latched(A1, T), m40(A1, T, M1)),
      NC = count(A2) over (latched(A2, T), m40(A2, T, M2), padj(A2, T, P2), P2 > crowd_mult * M2).

  rel dg_ratio(-A: Equity, @T: Timestamp, -R: Scalar)
  dg_ratio(A, T, R) :- universe(A, T), padj(A, T, P), m40(A, T, M), R = P / M.

  rel dg_padj(-A: Equity, @T: Timestamp, -P: Price<USD>)
  dg_padj(A, T, P) :- universe(A, T), padj(A, T, P).

  rel dg_close(-A: Equity, @T: Timestamp, -P: Price<USD>)
  dg_close(A, T, P) :- universe(A, T), close(A, T, P).

  rel dg_tr(-A: Equity, @T: Timestamp, -P: Price<USD>)
  dg_tr(A, T, P) :- universe(A, T), trclose(A, T, P).

  rel dg_split(-A: Equity, @T: Timestamp, -F: Scalar)
  dg_split(A, T, F) :- universe(A, T), split(A, T, F).

  rel dg_dvol(-A: Equity, @T: Timestamp, -D: Notional<USD>)
  dg_dvol(A, T, D) :- universe(A, T), dvol(A, T, D).

  rel dg_rowno(-A: Equity, @T: Timestamp, -X: Scalar)
  dg_rowno(A, T, X) :- universe(A, T), rowno(A, T, X).

  rel dg_score(-A: Equity, @T: Timestamp, -S: Scalar)
  dg_score(A, T, S) :- universe(A, T), score(A, T, S).

  rel dg_m40(-A: Equity, @T: Timestamp, -M: Price<USD>)
  dg_m40(A, T, M) :- universe(A, T), m40(A, T, M).

  rel dg_adv4(-A: Equity, @T: Timestamp, -D: Notional<USD>)
  dg_adv4(A, T, D) :- universe(A, T), adv4(A, T, D).

  rel dg_sp1500(-A: Equity, @T: Timestamp)
  dg_sp1500(A, T) :- universe(A, T), member(A, T, "SP1500").

  rel dg_spx(@T: Timestamp, -V: Scalar)
  dg_spx(T, V) :- series(T, "SPXTR", V).

  rel dg_vix(@T: Timestamp, -V: Scalar)
  dg_vix(T, V) :- series(T, "VIX", V).

  rel dg_vixma(@T: Timestamp, -V: Scalar)
  dg_vixma(T, V) :- series(T, "VIX_MA4", V).
}
