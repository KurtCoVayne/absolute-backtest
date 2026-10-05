# Briefs for the LLM-authored corpus

Plain-language briefs (data-bundle doc, section 8): each is given to a
model, which writes the strategy (and, where it says so, the study) in the
DSL against the corpus environments and libraries. Nothing in this directory
is asserted by a test; the harness measures and records.

Procedure: for a brief `NAME.md`, keep the model's successive attempts as
`attempts/NAME/1.dsl`, `attempts/NAME/2.dsl`, ... (one strategy unit each,
the model's text as written, nothing edited by hand), then run

```
abt briefs report --briefs briefs --attempts attempts [--out report.json] corpus/
```

The report records, per brief, whether the first attempt checked clean,
the diagnostics of every attempt (code and judgment), the attempts it took
to a valid program, and for the first valid one a run on the synthetic
market with the default cost model: decisions, fills, Sharpe ratio, CAGR,
maximum drawdown, the degrees of freedom and a verdict (`trades`,
`does not trade`, `does not survive costs`, `halted`). Across briefs it
reports the first-attempt pass rate, the distribution of diagnostics and
the mean attempts to a valid program: the evidence, over model versions,
that the language, the catalog and the warnings are doing their job. A
brief whose honest verdict is "does not survive costs" is the system
working.
