# The pandas reference

A second implementation of the execution contract of
`docs/semantic-model.md` section 6, written in plain Python over pandas
frames from the model's text and sharing nothing with the kernel
(data-bundle doc, section 8). It replays a run's decisions through its own
book, and `diff.py` compares the two. The run is read back from the store it
ran over: the store is the dump (M-9).

```
abt data ingest --store /tmp/st --adapter synthetic --class equity --days 400 corpus-v2/catalog corpus-v2/lib
abt run --strategy sma_crossover --data /tmp/st --quiet corpus-v2/catalog corpus-v2/lib corpus-v2/strategies/sma_crossover.dsl
python3 reference/diff.py /tmp/st RUN [--abt PATH] [--tolerance 1e-9]
```

`RUN` is the id `abt run` prints. `--abt` names the binary the store is read
with (default `$ABT`, else `abt` on the PATH).

- `store.py` reads the run back through `abt data query --store STORE ...
  --parquet`, as any reader of the store does, and writes the layout
  `engine.py` replays:
  - the binding (`kernel.<run>.binding`) gives the configuration (the
    resolved `execution` block) and the version of every relation the run
    read;
  - the price and volume relations, `split`, `dividend` and `delisted` are
    read at those versions, each instrument named by its `vendor_id` key, as
    the kernel names it;
  - the decisions, the events (fills, dropped decisions, actions), the book,
    the holdings and the report are read from `kernel.<run>.*`.
- `engine.py` replays the decisions.
- `diff.py` exports to a temporary directory and compares.

`python3 reference/store.py STORE RUN DIR` writes the export alone, and
`python3 reference/engine.py DIR` replays it.

Fills must be identical in quantity and in price to floating tolerance
(relative 1e-9), the book identical at the end, the NAV within tolerance at
every bar, and the dropped decisions equal in number. A disagreement is a
bug in one of the two, and the semantic model is the referee. One was found
while writing this: the kernel accrues funding at every bar, while the first
draft of the reference did so only on bars that followed a decision. The
model's text settled it, and it is now a sentence in `engine.py`'s notes.

`crates/abt/tests/reference.rs` (G6) runs each case over a store of its own
and compares it through `diff.py`. The cases are:
- every daily corpus strategy;
- a set of policy and model variants: Reg T, liquidation, clamp and reject,
  partial fills, fractional lots, fixed slippage with cash and rebate rates,
  frictionless;
- the four action markets: a split, a dividend, a spin-off as a dividend in
  kind, a delisting.

It skips, with a note, where `python3` with pandas is not installed.
`.github/workflows/ci.yml` installs pandas so the comparison runs in CI.

What the reference covers is the daily contract:
- next-bar fills at the close, lot rounding, delta and target sizing;
- the oversize, ruin and leverage policies;
- the participation cap with target re-issue;
- square-root impact, volatility-scaled slippage, commissions and fees;
- borrow buckets, funding, maintenance margin calls;
- splits, dividends, delistings.

What it does not cover:
- the minute contract (a decision filled at the last fine close of the next
  bucket);
- the feature library, which the corpus strategies exercise through the
  kernel's own evaluator. A pandas feature library is a further step of the
  realism program.
