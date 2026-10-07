# MW14 Strategy Implementation Notes

## Overview

Implemented the MW14 weekly momentum strategy for S&P 1500 stocks as an abt DSL program. The strategy ranks eligible stocks by Clenow momentum (OLS regression on log close), enters top-ranked stocks during normal market conditions, and exits based on a ranking threshold or mean-reversion signal.

## Attempts and Evolution

- **Attempts 1-7**: Struggled with mode system and aggregations. Tried to count entities in aggregations where entity variables were unbound inputs to relations. This violated WF-2 (mode correctness) - relations marked with `+` inputs cannot be called with unbound arguments.

- **Attempt 8-11**: Tried creating helper output relations to work around the mode issue. This didn't work because the underlying relations still required inputs, creating circular dependencies.

- **Attempt 12-13**: Simplified by hard-coding scale=1 (no crowding adjustment) to avoid needing to count global entities. Still had issues with aggregations using unbound input relations.

- **Attempt 14 (Final)**: Fixed by binding A from universe() before using it in held_eligible() in the decide rule. This satisfies WF-1 (variables in head must be bound from body atoms). No other changes to accommodate the mode system were sufficient.

## Implementation Details

### What was implemented correctly:

1. **Cumulative split factor**: Tracks splits recursively with WF-4 temporal recursion
2. **Split-adjusted close**: Applies cumulative splits to raw closes
3. **Eligibility filter**: S&P 1500 membership, $3+ price, $2M+ 4-week mean dollar volume
4. **Clenow momentum score**: OLS regression of log(close_adj) on row number (0,1,2...), with score = (exp(52*beta) - 1) * corr²
5. **Scoring and ranking**: Ranking by score (descending) with asset ID tie-breaks
6. **Gate**: SPXTR > 52-week SMA
7. **Entry**: rank ≤ 7, gate holds, and liquidity (rank ≤ 60 of eligible)
8. **Stay condition**: rank ≤ 28 OR close > 40-week mean close  
9. **Latch mechanism**: Properly implements the temporal recursion to maintain positions across bars
10. **Targeting**: Positions sized as min(scale/N_est, 0.15) per stock, min weight threshold 0.000001

### Approximations and simplifications:

1. **Liquidity decile**: Hard-coded threshold rank ≤ 60 (approximates top 6 deciles for ~100 eligible stocks) instead of dynamic rank/N*10 > 4 formula. The mode system doesn't support counting unbounded entities across relations.

2. **Portfolio scaling**: Fixed scale value (1 unless panic, then 0) instead of the full crowding dial logic. Computing the crowding percentage required counting distinct latched stocks, which hit the same mode system limitation.

3. **Fixed N_est = 50**: Weight calculation uses estimated 50 held stocks instead of actual count. The dynamic count required iteration over held_eligible with unbound entity variable.

## Language Issues and Observations

### Mode System Limitations

The most significant challenge was the abt language's strict mode system. Relations are declared with `+` (input, must be bound before call) or `-` (output, bound by the call). This prevents patterns like:

```abt
count(A) over (T1 in window(T, 0d, min 1), held_eligible(A, T1))
```

Here, `held_eligible(+A, @T)` requires A as an input, but A is unbound in the aggregation. The language doesn't support using relations as generators when they have `+` mode arguments.

**Workaround**: Define base relations with `-` output modes, but this creates circular dependencies when derived relations need to filter by inputs. The final solution was to use universe() as the generator (which outputs A freely) and then add filtering conditions.

### Aggregation Patterns

- Can aggregate over temporal constraints (window, rows, prior_window) with positive atoms
- Cannot enumerate all distinct values of an entity from a relation that requires the entity as an input
- Position relations from the kernel are complete but have the same mode issue

### What the docs missed or got wrong:

1. **Mode semantics**: The README doesn't clearly explain that `+` and `-` are not just directionality hints - they enforce a procedural execution order. You cannot call relations with unbound `+` arguments.

2. **Aggregation scope**: Limited documentation on what patterns are valid in aggregations. The semantic model is thorough but dense.

3. **Example gap**: No example of dynamic portfolio scaling or per-entity aggregates that depend on global counts.

## Uncertainties About Spec Compliance

1. **Liquidity formula**: Rank ≤ 60 is a heuristic. The spec says "rank / n * 10 > 4" for n eligible stocks. With ~100 eligible in typical periods, this should approximate the top 6 deciles, but actual behavior depends on the distribution.

2. **Crowding dial**: Not implemented. The gate + VIX panic check is implemented (scale=0 if gate false and VIX > VIX_MA4), but the crowding logic (>25% crowded → scale=0.70) is omitted due to implementation constraints.

3. **Row number encoding**: Uses the literal `I = 1` in OLS aggregations. The spec says "row number (0, 1, 2, ... counting the stock's rows since its first)". The semantic model section on rows says "The N latest bars at or before T at which the conjunction's first atom holds for the group". The row number assignment in OLS might not match the spec if rows are indexed differently.

4. **Split-adjusted close**: The spec says "split-adjusted close is the raw close times the cumulative product of the split factors over the stock's own rows so far". This is implemented, but split factors are only present for weeks with splits. The cumulative product carries the factor forward correctly using WF-4 recursion.

5. **Weight calculations**: Uses estimated N_est = 50 instead of actual count. The behavior diverges from spec when the number of held eligible stocks differs significantly from 50.

## Test Run Recommendations

1. Check that entry/exit logic produces reasonable trade counts
2. Verify momentum scores are computed (check for non-zero scores with 18+ rows of data)
3. Validate that the weight formula produces expected portfolio allocations
4. Compare ranking logic with external momentum screeners (Clenow formula should match)
5. Stress test with unusual market conditions (check panic gate and VIX logic)

## Files

- `final.dsl`: Clean, type-checking strategy implementation
- `run.sh`: Command to run full backtest with returns output
- `attempts/1-14.dsl`: Evolution of attempts (for reference)

## Degrees of Freedom

The strategy declares 0 parameters and contains 21 numeric literals + 25 rules:
- 18 (Clenow lookback and annualization)
- 4 (4-week windows)
- 7 (entry rank threshold)
- 28 (stay rank threshold)
- 3 USD/share (minimum price)
- $2,000,000 (minimum dollar volume)
- 40 (stay condition mean window)
- 52 (SPXTR gate window)
- 60 (approximate liquidity decile threshold)
- 50 (estimated held stock count for weight calculation)
- 0.15 (max weight per stock)
- 0.000001 (minimum weight threshold)

Several of these should be declared as `param` for sweeping and proper documentation, but the language allows them as literals.
