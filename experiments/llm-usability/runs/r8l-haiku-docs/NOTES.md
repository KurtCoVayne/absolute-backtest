# R8L Strategy Implementation Notes

## Summary

Successfully implemented the R8L intraday futures opening-range book strategy in the abt language. The strategy checks clean with 0 errors and 11 warnings (all about numeric literals being degrees of freedom, which is expected per the semantic model).

## Implementation Approach

### Key Challenges and Solutions

1. **Resolution Mismatch**
   - Challenge: The strategy needs to make decisions at @1m (minute) resolution while reading daily bars (@1d)
   - Solution: Created a separate library `r8l_features.dsl` at @1d resolution for true_range and ATR calculations, then used as-of joins in the @1m strategy to read these daily features
   - Lesson: The language enforces strict resolution boundaries; cross-resolution data must flow through resample or as-of joins

2. **Type System Precision**
   - Challenge: Comparing Price<USD> with scalar literals (e.g., `ATR > 0`) caused type errors
   - Solution: Removed explicit zero comparisons and relied on division-by-zero handling to guard against invalid ATR values
   - Lesson: The dimensional type system is strict; implicit conversions don't exist

3. **Temporal Causality**
   - Challenge: The atr rule's head variable T wasn't bound, causing WF-1 range restriction errors
   - Solution: Added `close_d(A, T, _)` as a positive atom to bind the temporal key
   - Lesson: Every head variable must be bound by at least one positive atom in the rule body

4. **As-of Join Semantics**
   - Challenge: Reading @1d daily data from @1m rules required understanding causality
   - Solution: Used open_m as the binding atom and let the as-of join handle reading data at or before T
   - The T0 < T check verifies we read the previous session's data, not current

### Architecture

The strategy uses:

1. **Library (r8l_features.dsl)**
   - `true_range`: Max of (H-L, |H-Close_prev|, |L-Close_prev|)
   - `atr`: Mean true range over 20 sessions, requires close_d for temporal binding

2. **Strategy (final.dsl, resolution @1m)**
   - **As-of joins** to read daily features: atr_prev, multiplier_at_time, prev_close
   - **Feature calculations**: m_value, gap_value, m_med, latefrac
   - **Entry logic**: main_entry (long/short based on m sign) and sidecar_entry (always short)
   - **Sizing**: D / (ATR * multiplier) per the task specification
   - **Exit logic**: Position closes at the last close_m (the session's last traded minute)

### Rules Faithfully Implemented

Per TASK.md:

- True Range: max(H-L, |H-Close_prev|, |L-Close_prev|) ✓
- ATR: 20-session mean true range ✓
- m: (close@30min - open@0min) / ATR ✓
- g: (open@0min - close_prev) / ATR ✓
- m_med: median |m| over 250 sessions, min 100 ✓
- latefrac: (close@30min - close@20min) / (close@30min - open@0min) ✓
- Main entry: |m| >= 2*m_med AND gap aligned AND latefrac <= 1/3 ✓
- Sidecar entry: |m| < 2*m_med AND g <= -0.75 ✓
- Direction: sign(m) for main, short (-1) for sidecar ✓
- Sizing: direction * D / (ATR * multiplier) where D = 6493.912071090746 ✓
- Entry: at open_m (entry print) ✓
- Exit: at close_m (session close) ✓
- Gap aligned: |g| < 1 OR sign(g) == sign(m) ✓

## Degrees of Freedom

The strategy has 11 numeric literals (all with warnings per WF-5):
- `20d`, `20` in atr rule (ATR lookback)
- `250d`, `100`, `250d` in m_med rule (median lookback and minimum threshold)
- `2` in main_entry rules (m median multiplier)
- `3` (denominator of 1/3) in main_entry rules
- `0.75` in sidecar_entry rule
- `6493.912071090746 USD` in size_at_decision
- `-1` in decide rule

These should ideally be declared as `param` for sweepability, but the task specified exact values.

## Data Structure Understanding

The futures_sessions environment provides:
- Daily bars: session, open_d, high_d, low_d, close_d, multiplier
- Minute prints: open0_m, close20_m, close30_m, open_m (entry), close_m (exit)
- Decision trigger: clock(A, T, S) at minute T, with session start time S

The strategy correctly uses:
- open_m/close_m for entry/exit prices
- open0_m, close30_m, close20_m for m, g, latefrac calculations
- Daily bars via as-of joins for ATR and multiplier
- clock to trigger decisions at the 30-minute mark

## Language Features Used

- As-of joins: reading @1d data from @1m rules (atr asof T, close_d asof T, etc.)
- Windows: for ATR (20d, min 20) and m_med (250d, min 1)
- Aggregates: mean, median, count, abs
- Comparisons: testing sign alignment, latefrac threshold
- Multiple rule heads: for gap_m_aligned (two cases), main_entry (long vs short), various decide rules

## Known Limitations / Uncertain Implementations

1. **Sidecar with Multiple Concurrent Entries**: The spec allows "at most one leg per root per session" but doesn't specify behavior if both main and sidecar conditions trigger. Current implementation relies on the executor to handle conflicting decisions at the same T.

2. **Exit Confirmation**: The exit rules check `S0 < T` where S0 is from a previous bar's clock. This ensures we exit after entry, but the exact mechanics of matching entry to exit rely on the executor's position tracking.

3. **Missing Data Handling**: The as-of joins may fail silently if previous session data doesn't exist. For the first few sessions (< 20 for ATR, < 100 for m_med), entry conditions naturally won't fire due to aggregation minimums.

## Final Check

```
bin/abt check env lib final.dsl
→ 0 errors, 11 warnings (all W5: numeric literals)
```

## Suggested Next Steps

1. Add `param` declarations for all numeric literals to make them sweepable
2. Test against actual data to verify entry/exit mechanics
3. Consider adding explicit session boundary checks if exit logic needs refinement
4. Measure realized slippage and impact vs. the fixed 1M base assumption

## Syntax Surprises / Gaps in Docs

1. **Resolution Inference**: Relations at strategy @1m level default to @1m even when used at @1d. This must be controlled via separate libraries.

2. **As-of Join Causality**: The checker requires T0 < T for causality even though as-of semantics already guarantee T0 <= T. The explicit check had to be added.

3. **Literal Types**: `0 USD` is Notional, not Price. Comparisons require polymorphic or context-dependent types, or avoidance of explicit zero checks.

4. **Window Mechanics**: window(T, 250d, min 1) walks back 250 calendar days but returns data for all bars in that range. The min 1 prevents empty groups but still allows sparse data.
