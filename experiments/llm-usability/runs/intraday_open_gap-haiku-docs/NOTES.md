# Intraday Open Gap Strategy - Implementation Notes

## Summary

This strategy trades on intraday opening gaps in minute-bar data. It checks if the first minute's close is more than 1% above the previous day's close, then buys at the second minute and sells at the day's close.

## Final Results

- **Status**: Checks clean, runs successfully on synthetic data
- **Test data**: 30 days of SPY synthetic data
- **Performance**: -0.04% total return, 0.04% max drawdown
- **Trades**: 3 trades (3 gap-up events, each with buy+sell)
- **Fills**: 6 total (3 buys + 3 sells, all executed)

## Implementation Details

### Core Logic

1. **Gap Detection**: At the first minute of each day (identified via `day_start(T)`), compare the minute's close to the previous day's close using an as-of join (`close_d(A, T0, P0) asof T`). The gap is > 1% if `close_m(A, T, P1) > close_d(A, T0, P0) * (1 + 0.01)`.

2. **Buy Signal**: At the second minute of the day (identified via `prev(T, T0)` where `first_min(T0)`), buy 100 shares if flat.

3. **Sell Signal**: Issue sell orders throughout the day when holding a position, using `moc` (market-on-close) order type to ensure execution at the session close.

### Key Design Decisions

- **Parameters**: Made gap_threshold (default 1%) and buy_qty (default 100 shares) into parameters for tunability
- **Order Type**: Used `moc` to ensure sells execute at the session close, matching the task requirement to "sell at the day's last minute"
- **Completeness Requirement**: Created a derived relation `first_min` to wrap the `day_start` temporal builtin, since builtins cannot be negated directly (WF-5 completeness)
- **Multiple Sell Orders**: The strategy issues multiple sell decisions (one per minute while holding), each with moc. The executor supersedes earlier orders with later ones, but this doesn't affect correctness, only efficiency.

## Challenges and Solutions

### Challenge 1: Negating Temporal Builtins
The checker initially rejected `not day_start(T)` because builtins cannot be negated directly (WF-5 completeness). 
- **Solution**: Created a derived relation `first_min(T) :- universe_m(_, T), day_start(T)` and negated that instead: `not first_min(T)`.

### Challenge 2: Multiple Sell Decisions
An earlier optimization attempt (v4) tried to issue only one sell decision per position by checking if the position increased. However, this broke the sell logic because the timing of position updates relative to decision evaluation was unclear.
- **Solution**: Reverted to the simpler approach (v3) that issues multiple sell orders. The executor handles superseding them correctly, and correctness is more important than efficiency in the minimal test case.

### Challenge 3: Identifying "Last Minute" of Day
The task requires selling at the "day's last minute", but the language provides limited ways to identify the exact last minute without accessing the next day.
- **Solution**: Used `moc` (market-on-close) order type, which automatically fills at the session close regardless of when the decision is issued. Paired with issuing sell orders throughout the day while holding, this ensures execution at close.

## Language Observations

### What Worked Well
- The as-of join (`asof`) for cross-resolution reads (reading previous day's close from within a minute-bar strategy)
- Dimensional type system caught issues and guided correct implementation
- Order types (moc) provided the control needed for intraday strategies
- Parameterization of literals made the strategy flexible

### Gaps or Unclear Aspects
1. **Builtins and Completeness**: The language doesn't allow negating temporal builtins directly, requiring a workaround. The docs could clarify this pattern more explicitly.
2. **Position Timing**: The semantics of when `position` is updated relative to decision evaluation could be clearer. Determining exactly when a position reflects a fill requires careful reading of the execution contract.
3. **Multiple Order Superseding**: The behavior when multiple orders for the same instrument exist (e.g., multiple moc orders) could be documented more explicitly.

## Files

- **final.dsl**: The final working strategy
- **run.sh**: Command to run the strategy on synthetic data
- **attempts/1.dsl - 3.dsl**: Earlier attempts showing iteration
- **NOTES.md**: This file

## How to Run

```bash
cd /private/tmp/claude-501/.../<workspace>/
./run.sh
```

Or with different parameters:
```bash
./bin/abt run --strategy intraday_open_gap --synthetic --days 30 --symbols SPY,QQQ --seed 42 \
  --param gap_threshold=0.015 --param buy_qty=50 env lib final.dsl
```
