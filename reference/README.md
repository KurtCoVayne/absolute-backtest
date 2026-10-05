# The pandas reference

A second implementation of the execution contract of
`docs/semantic-model.md` section 6, written in plain Python over pandas
frames from the model's text and sharing nothing with the kernel
(data-bundle doc, section 8). It replays the decisions of a kernel dump
through its own book and `diff.py` compares the two:

```
abt run --strategy sma_crossover --synthetic --quiet --dump /tmp/sma corpus/
python3 reference/diff.py /tmp/sma
```

Fills must be identical in quantity and in price to floating tolerance
(relative 1e-9), the book identical at the end, the NAV within tolerance at
every bar, and the dropped decisions equal in number. A disagreement is a
bug in one of the two and the semantic model is the referee; the one found
while writing this (the kernel accrues funding at every bar, the first
draft of the reference only on bars that followed a decision) was settled
by the model's text and is now a sentence in `engine.py`'s notes.

`tests/reference.rs` runs every daily corpus strategy, a set of policy and
model variants (Reg T, liquidation, clamp and reject, partial fills,
fractional lots, fixed slippage with cash and rebate rates, frictionless)
and the four action markets (a split, a dividend, a spin-off as a dividend
in kind, a delisting) through the comparison; it skips, with a note, where
`python3` with pandas is not installed. The workflow in `ci/` installs
pandas so the comparison runs in CI once it is moved under
`.github/workflows/`.

What the reference covers: the daily contract (next-bar fills at the close,
lot rounding, delta and target sizing, oversize, ruin and leverage
policies, participation cap with target re-issue, square-root impact,
volatility-scaled slippage, commissions and fees, borrow buckets, funding,
maintenance margin calls, splits, dividends, delistings). What it does not:
the minute contract (a decision filled at the last fine close of the next
bucket) and the feature library, which the corpus strategies exercise
through the kernel's own evaluator; a pandas feature library is a further
step of the realism program.
