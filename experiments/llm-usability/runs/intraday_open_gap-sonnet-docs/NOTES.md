# Notes: intraday_open_gap

Attempts: 3 (1 and 2 check clean; 3 is final). About 5 bin/abt invocations.

## Design
- Decides at @1m. Gap test at day_start(T) (first minute): close_m > close_d * (1+gap), with close_d read by
  `asof T` from the bars library (the previous session's daily bar, per the docs).
- buy at the first minute's decision, which fills at the next bar's close (the second minute): the "market" contract.
- sell(A,Q,moc) decided at the second minute (held_m and prev(T,T0), first_min(T0)); moc fills at the session's last bar close.

## Hard / unclear
- "Buy at the second minute": the docs say decisions fill at the next bar's close, so deciding at minute 1 fills at minute 2. I inferred this; no explicit statement that fill price = close of the second minute.
- Whether position() already shows the buy at the second minute's step (needed for the sell rule) was not documented; attempt 3 shows it does (102 fills = 51 round trips).
- Attempt 1/2 emitted a sell decision every minute while held (superseded silently); I restricted to the second minute.
- `moc` syntax appears only in an example for target_weight; extending it to delta `sell(A,Q,moc)` was a guess (worked).
- No docs on how to say "no sell when buy didn't fill".
- Comments with // worked (guessed).

## Unsure of fidelity
- Position size: 100 shares per name (brief gives none); every gapped symbol in the universe trades.
- Gap uses the previous session's close_d (min 300 bars rule: short sessions yield no close_d, so asof reads an older day).
- Strictly more than 1% (P > C*(1+gap)).

## Results (synthetic, 60 days, default seed)
51 round trips, all losers (win_rate 0), total_return -1.18%, max_drawdown 1.18%, impact costs 11.5k (dominant), commissions 102.
Sharpe per-bar -15 (meaningless at minute annualisation).
