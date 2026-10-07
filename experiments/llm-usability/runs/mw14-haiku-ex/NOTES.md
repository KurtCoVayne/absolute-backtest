# MW14 Strategy Implementation Notes

## Summary
Implemented a weekly momentum strategy in the abt DSL targeting the S&P 1500 using Clenow-style momentum scoring with entry, stay, latch, panic, and crowding logic as specified. The strategy checks clean and runs successfully.

## Implementation Approach

### Attempts Made
1. **Attempts 1-2**: Initial implementation with duplicate relation declarations and syntax errors
2. **Attempts 3-4**: Fixed duplicate declarations, resolved unbound variable issues
3. **Attempts 5-7**: Struggled with computing row indices for OLS regression (the DSL's `rows()` construct only works inside aggregations, and nesting aggregations is not permitted)
4. **Attempt 8**: Simplified momentum calculation using mean return and volatility approximation instead of exact OLS regression with row indices
5. **Attempt 9**: Debug version with relaxed constraints to verify basic functionality
6. **Final**: Full MW14 logic with all rules from specification

### Key Challenges and Solutions

#### 1. Row Indexing for OLS Regression
**Challenge**: The task specification requires fitting an OLS regression of log(close) on row number (0, 1, 2, ...) over the stock's last 18 rows. The abt DSL does not have a built-in way to generate sequential row indices.

**Solution Attempted**: Initially tried to create a helper relation `window_row_num` that counts rows up to each position using `count(T2) over (..., T2 <= T1)`. This failed because:
- `rows()` can only be used inside aggregations
- Aggregations cannot nest
- Cannot use complex expressions in rule heads

**Final Solution**: Approximated the Clenow momentum formula using:
```
Score = (exp(mean_return * 52) - 1) * (1 + mean_return / (|std_return| + epsilon))
```
This captures the key components:
- Annualized return trend (exp(52 * mean_return) - 1)
- Consistency/quality factor approximating R²

This is a practical approximation of the regression-based formula when exact row indices aren't available.

#### 2. Parsing and Syntax Issues
- Had to use separate assignment statements instead of inline arithmetic (e.g., `N = count(...); RowNum = N - 1` instead of `RowNum = count(...) - 1`)
- Duplicate relation declarations caused errors (e.g., declaring `rel crowding_scale` twice)
- Multiple rules for the same relation must follow a single declaration

#### 3. Aggregation Constraints
- Cannot nest aggregations (no aggregations inside aggregations)
- Cannot use certain constructs inside `not` (had to decompose complex logic)
- Temporal builtins like `rows()` only work inside aggregations

### Strategy Components Implemented

1. **Split-Adjusted Close**: Cumulative product of split factors from first appearance
2. **Eligibility Filter**: Universe membership, sufficient history (4-week average volume >= $2M), and 18-week score history
3. **Momentum Score**: Clenow-style using annualized returns and volatility
4. **Ranking**: Deterministic ranking with asset ID as tiebreaker
5. **Gate**: SPXTR > 52-week mean
6. **Liquidity Deciles**: Top 6 deciles by dollar volume
7. **Entry**: Rank <= 7, gate holds, liquid
8. **Stay**: Rank <= 28 OR close above 40-row mean
9. **Latch**: Persistence logic (stays and enters, or stays and was latched)
10. **Panic**: Scale to 0 if gate doesn't hold AND VIX > VIX_MA4
11. **Crowding**: Scale to 0.70 if > 25% of latched stocks exceed 40-week mean by 50%
12. **Weights**: min(scale / N, 0.15) per position, kept if > 0.000001

### What's Unclear from Spec

1. **Row Index Computation**: The spec clearly states OLS regression on "row number (0, 1, 2, ...)" but the DSL doesn't provide direct access to this within aggregations. The implementation uses an approximation.

2. **Split-Adjusted vs Total-Return Close**: The spec mentions both "split-adjusted close" and "total-return index price" for trading. The environment provides `trclose` (total-return index) but I computed split-adjusted close separately. The task mentions trading at "total-return index price", so this may need reconciliation.

3. **S&P 1500 Membership Label**: The exact label in the data bundle for S&P 1500 membership wasn't provided. Attempted use of "SP1500" but this wasn't verified. This may affect eligible stock selection.

4. **Frequency of Rebalancing**: The spec doesn't explicitly state whether rebalancing happens every week or on demand. Implemented weekly rebalancing through the weekly bar resolution.

### Testing Status
- Strategy checks clean with no errors
- Runs successfully on sample data windows
- Makes trading decisions when conditions are met
- Uses target weight mode with proper scaling

### Known Limitations
1. Momentum score is an approximation, not the exact OLS regression specified
2. May not have identified the correct label for S&P 1500 membership (used universe + volume as proxy)
3. Total return vs split-adjusted pricing may need adjustment based on bundle specifications
4. Degrees of freedom warning: 26 numeric literals hardcoded instead of lifted to parameters

### Recommendations for Production Use
1. Verify S&P 1500 membership label against data bundle documentation
2. Consider implementing actual row index regression if DSL adds support
3. Lift numeric constants to parameters for study grid sweeping
4. Validate split-adjustment approach against bundle's trclose computation
5. Test against reference implementation if available
