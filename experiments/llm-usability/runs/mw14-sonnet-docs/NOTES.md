# MW14 notes

Attempts: 3 files (1: first draft, 2: fix modes, 3: params, clean). 5 bin/abt calls plus 1 for the full run. final.dsl = attempts/3.dsl; checks with 0 errors, 0 warnings.

## Hard / guessed
- No cumulative-product aggregate and no row-number primitive: built the stock's own-row recursion via `prev(T,Tp)` + `close(A,T0,_) asof Tp` (latest own row strictly before T), used for cumulative split factor, row index (rown) and the latch recursion. Works, but is an idiom I inferred from the as-of section.
- Rank is Scalar; `K / N * deciles > liq_cut` with N Count compiled. `log(P / 1 USD/share)` compiled (log of price must be divided by a unit price).
- Entity-less `rows(T, 52, min 52)` over `series(...)` for the SPXTR gate: guessed it works (checker accepted; not verified numerically).
- Relations that must be enumerated need `-A` in the signature (first draft used `+A` and got M errors); docs say little on when to use which.
- Banding: target mode, decide only when the band test passes, rely on "no decision = keep position"; sells are target_weight 0. moc order type to fill at the decision close. Guessed that omitted decisions leave the position untouched (consistent with the run, not documented explicitly).
- `--start 1991-01-01` trades only from 1991 (the usage has no separate report-from); latch state is still derived from 1990 data, so names latched earlier are bought at the first 1991 bar if target >= 2.5%. A book that traded from 1990 would differ slightly.
- Cost flags: --commission-bps 10 with per-share commission/min/fee/slippage/impact/participation zeroed, margin and cash rates 0, --on-leverage allow, --delist-proceeds last-price. The run still prints "warning: transaction cost neglect: commissions zero" though commissions of 216,975 are charged (the warning ignores bps).
- The docs README excerpt says docs/data-bundle.md exists; it does not in the workspace.

## Unsure of faithfulness
- Split-adjusted close includes the current week's split factor (as catalog library does).
- "Tie rank average" liquidity rank over all eligible stocks (not just scored ones).
- Scale 0 when panic: no targets so all held names are sold (immediately, no band).
- A latched stock not eligible this week stays latched but gets no weight; held position sold since no target.
- Gate is false when fewer than 52 SPXTR rows exist.

## Results (1991-01-04 to 2026-07-31, 1856 weekly periods)
CAGR (compounded series) 20.6%, Sharpe 1.01, vol 20.6%, max DD 26.2%; additive annual return 20.9%, total PnL 7.47M on 1M base, 1946 trades, win rate 50.3%, 149 delistings, costs 217k. Full run takes ~6 minutes (docs said ~2). Returns saved to returns.csv.
