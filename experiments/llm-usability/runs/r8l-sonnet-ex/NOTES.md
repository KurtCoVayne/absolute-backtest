# r8l notes
Attempts: 1.dsl (5 checker errors: modes of -A vs +A), 2.dsl = final.dsl (checks clean, 0 warnings). 7 abt invocations in all.

## Design
- Strategy decides at @1m on `clock` (decision time). Daily features are @1d derived relations (tr, atr, absm, mmed) using
  `rows(T,N,min K)` over `session(A,T1)` so each root uses its own sessions (the global @1d time domain is the union of
  all roots' dates, so `prev` alone is not the root's previous session; I use `session(A,TP) asof T1` after `prev`).
- Minute prints per session via `resample(... to @1d ...)` for the daily m history, and `asof` reads at decision time.
- "Previous session, missing not carried over": asof carries older tuples, so I bind the key of each asof read and
  require it to equal the key of `session(A,TS) asof T` (the previous session). Likewise `S < K` guards that a minute
  print belongs to the current session (S = clock's session open).
- Orders: `buy/short(A, Q, moo_moc)`; sizing Q = risk / (ATR * multiplier).
- Division by zero guard: `abs(D) > 0` before latefrac (spec silent; D=0 means no main trade, and sidecar also skipped
  since it shares sig). SPEC DEVIATION RISK: sidecar needs only |m|<2 m_med and g<=-0.75, and with D=0 m=0 it would
  qualify in the spec but my `sig` requires D!=0 so it is dropped. Rare.

## Hard / undocumented
- Which price relation the executor uses: needed `--price-relation close_m`; moo/moo_moc then use open_m implicitly (guessed;
  without it every order dropped "did not open"). Docs never say open_m is the open source for moo.
- Default margin halts the run; needed `--max-gross 1000 --on-margin-call allow --on-leverage allow`. Default
  fee-bps 0.278 on sells and margin-rate 5% had to be zeroed (`--fee-bps 0 --margin-rate 0 --cash-rate 0`).
- Whether asof accepts a bound T0 as an equality check: untested; I used a separate variable and `=`.
- Types: `greatest` with 3 args accepted; `0 USD/share` literal for comparisons; `0 - side_gap` for negative param.
- Unsure: that the clock decision time + moo_moc fills at exactly the entry print (first traded minute from minute 30)
  and exits at the session's last print; I did not inspect fills.parquet. Also unsure whether `moo` could skip to a
  different minute when the root has no bar at T+1 (no drops occurred in the final run).
- Resample buckets by calendar date of label; fine if sessions do not cross midnight.

## Results (run.sh)
21736 decisions, 43472 fills, 0 dropped. Sharpe 1.25, CAGR 19.0% (compounded-base metric), vol 14.7%, max DD 17.8%,
total pnl 5.02M additive on 1M base (additive annual return 18.5%), commissions 1.01M, win rate 52.8%.
