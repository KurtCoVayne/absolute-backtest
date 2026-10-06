# MW14 notes

Attempts: 5 (attempts/1..5.dsl); final.dsl = attempts/5.dsl, checks with 0 errors, 0 warnings. About 9 bin/abt invocations.

## Headline (full run, 1991-01-01 on, 1856 weekly periods)
CAGR 20.6%, vol 20.6%, Sharpe 1.01, max DD 26.2%, 1946 trades, commissions 217k on 224M turnover. Full run ~4 min.

## What was hard
- Own-row recursion: "latched at its own previous row" and row numbers / cumulative split factor over a stock's own rows.
  `prev` is the global previous bar, so I used `prev(T,Tp)` + `R(A,_,V) asof Tp` to reach the stock's own latest row.
  TRAP: `lstate(A,_,1) asof Tp` with a constant matches only rows where the value is 1, so it silently walks back to an OLD
  latched row (wrong semantics and a run that never finished, >20 min). The fix is to bind the value (`F0`) and test `F0 = 1` after.
  The docs say "latest key with a matching tuple" but do not warn about this consequence.
- No conditional expression and `not` cannot sit in a cycle: the latch state had to be written as 5 positive rules with explicit
  0/1 values, plus a `hasprior` helper.
- No cumulative product: split-adjusted close built from a recursive running sum of log split factors.
- Count vs Scalar: `Count * Scalar` and Scalar-from-Count are rejected; `H / N` (Count/Count) and `K * (1 / N)` work.
- `target` is a reserved word (error message was clear). Division by zero halts the run (crowded with N=0): needs `N > 0` guard.
- Tool: `bin/abt check` gives clean messages. Runtime was slow only because of the asof bug.

## Docs lacked / guesses
- rows windows: guessed `T1 in rows(T, N, min K), atom(...)` with the window first; it worked, also for an entity-less relation (`series`).
- rank by group: guessed grouping is by outer-bound T (`bar(T), rank(rel(A,T,S), by (...), as K)`); works. `ties average` syntax used as documented.
- Run flags: the default report is 252 periods/yr unless `--periods-per-year 52`; margin interest (5%) is charged by default unless `--margin-rate 0`; had to discover from output. `--start 1991-01-01` is how I read "report from 1991".
- `moc` order type used so fills happen at the decision week's close at the trclose price.
- The doc claims 15 s for a short window; my rules took ~30 s for 1990-95.

## Unsure about faithfulness
- Latch for stocks with gaps in their rows: uses the stock's own latest earlier row (asof), as specified; not tested on data with gaps.
- With --start 1991-01-01 the book is flat at the start; latched state from 1990 is computed from data but positions only start in 1991 (names already latched are bought at the first 1991 week).
- Stay's second clause (above 40-row mean) does not require eligibility or a score, as written.
- Delisting: `--delist-proceeds last-price`; cost on delist close not charged (per that mode).
- Liquidity rank: n counts all eligible stocks (not only those with a score).
- No cross-check against a reference implementation was possible.
