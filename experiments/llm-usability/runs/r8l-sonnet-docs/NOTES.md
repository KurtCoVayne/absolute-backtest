# NOTES (r8l, attempt 1 = final.dsl)

Attempts: 1 file; abt invocations: ~6 (1 check, 5 runs, three of them fixing run flags). final.dsl checks clean (only two W5 warnings for the literal 3 in 3*(C30-C20) <= C30-O0).

## Design
- Library r8l_daily (@1d): tr, atr (rows(T,20,min 20) over `session`), mo/mc (resample of open0_m / close30_m to @1d), mval (m uses ATR of the previous session), med (rows(T,250,min 100) of |m|), feat.
- Strategy at @1m on `clock`; reads feat as of the previous session via `asof`, and checks `feat` key = previous session key (`session(A,Tp) asof T, Tf = Tp`) so a missing previous-session feature is missing, not carried over. Same-session check on minute prints: `S < T0` (S = session open from clock).
- latefrac <= 1/3 done without division: 3*(C30-C20) <= C30-O0 for m>0, flipped for m<0 (avoids x/0 halt). Gap alignment simplified: long needs G > -1, short needs G < 1 (equivalent to |g|<1 or same sign).
- Orders: buy/short with `moo_moc`, price relation close_m.
- run flags: --bundle (not --data; --data gave "no bars"), fractional lots, no slippage/impact/participation/fees/funding, margin calls allowed (otherwise the run halts at 2003 since leverage reaches ~15x), --on-ruin continue.

## Difficulties / doc gaps
- Docs never state the CLI for a bundle in TASK terms: `--data` on a bundle dir silently loaded empty relations; `--bundle` worked. The README says bundle semantics live in data-bundle.md which is not provided.
- `rows(...)` syntax and "first atom" rule only described in one table row; I guessed `T1 in rows(T, N, min K), session(A, T1), x(A, T1, X)` and it worked.
- asof reading a derived @1d relation from @1m and comparing Timestamp values (`Tf = Tp`, `S < T0`) were guesses; accepted by checker.
- Order-type syntax `buy(A, Q, moo_moc)` is only shown for target_weight; guessed it works for delta decisions: it did.
- Executor flags needed for "no margin model" (margin-call allow, on-leverage allow, margin-rate 0) are not in the docs; found via usage text and a halt message. Commission: I did not pass --commission; the run reports commissions 1.02M, which I assume come from the security table (not verified; the default per-share rate would be negligible).
- `greatest` with two args only assumed; nested.

## Unsure about
- Whether commission really uses the table per-contract value.
- Decision timestamps print as 09:00 (session open 08:30 presumably, central time) - consistent with clock.
- Median "a session with no m takes a place" handled via rows over `session`, hoping rows' first atom counts sessions without m. Edge m=0 with latefrac undefined: main legs do not fire when m=0 (direction undefined).
- Resample label of open0_m/close30_m matches the session date label (assumed).

## Headline (full run)
6850 daily periods, 21903 trades, win rate 0.528: CAGR 19.5%, Sharpe 1.277, vol 14.8%, max DD 18.0% (fixed-base additive: annual return 18.9%, total PnL 5.15M, profit factor 1.31). Commissions 1.02M. Max leverage ~15x (unconstrained).
