# NOTES: momentum_liquid_monthly

Attempts: 4 (attempts/1-4.dsl); about 11 bin/abt calls. final.dsl = attempts/4.dsl, checks clean (0 errors, 0 warnings).

## Implementation
Target mode, env equities_1d_v2, `uses catalog`. At `month_start` bars: candidates are index members (`member(A,T,idx)`) that are `liquid` (catalog: 20d ADV >= 1M USD). Momentum = sum of catalog `logret` (total return incl. dividends/splits) over window 12mo minus the sum over the last 1mo (i.e. the 12-1 return). `top(10, ..., by (M desc, A asc))`, weight 0.1 each; held names not selected get target_weight 0.

## Hard / docs lacking
- The index label for `member` is not documented anywhere. "SP500" silently gave zero decisions (no error, no warning); I found "SPX" by trial and error with --param. Biggest difficulty. An empty-result diagnostic would help.
- The docs do not say whether target mode flattens unlisted names; I emit explicit weight-0 decisions. `not selected` was accepted by the checker (selected derives from complete `member` and universe... accepted anyway).
- There is no product aggregate, so a compounded 12-1 total return is done as a sum of log returns (equivalent to the product). The window boundary semantics ((T-N, T] vs [T-N, T]) are not stated; the "skip" is approximated by subtracting the last 1mo window.
- Min-observation counts (200 over 12mo, 15 over 1mo) are my guesses, lifted into params to avoid W5.
- Library `liquid` hardcodes 20d/10 obs window; the brief only says "average daily volume".

## Unsure
- Whether the 12mo-1mo window boundary is exactly faithful; the 1 holding of a name delisted (1 delisting occurred), 4 dropped decisions (cause not investigated, likely participation cap / delisting).
- Rebalance on month_start bar, filled at next bar close (default market).

## Results (synthetic, 5 symbols, 500 bars)
total_return -9.6%, cagr -5.0%, sharpe -0.50, max_dd 16.0%, 65 decisions, 61 fills, 4 dropped, turnover 887k. Only 5 symbols, so "top 10" holds everything eligible; not very meaningful.
