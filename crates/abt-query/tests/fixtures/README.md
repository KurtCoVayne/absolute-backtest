# Known-answer fixtures (G9, Q-4.6)

The data the starting set's known answers are checked against, each with
where it was taken from and its SHA-256. NIST's own site
(www.itl.nist.gov) is not reachable from the build container, so NIST's
files come from public mirrors that copy them unchanged, NIST's header with
the certified values included (ERRATA E-24).

| File | Source | SHA-256 |
| --- | --- | --- |
| `nist/Norris.dat` | NIST StRD, via scipy: https://raw.githubusercontent.com/scipy/scipy/main/scipy/stats/tests/data/nist_linregress/Norris.dat | `cc3fd14d1c5fa891d5653000c9d7732c30db842cca49fc051abde1c19d67ab7d` |
| `statsmodels/longley.csv` | NIST StRD's Longley data, via statsmodels: https://raw.githubusercontent.com/statsmodels/statsmodels/main/statsmodels/datasets/longley/longley.csv (the certified values as statsmodels transcribes NIST's, in `statsmodels/regression/tests/results/results_regression.py`) | `0927ec7cc34edb5670920cb2ff1542e46de27a2010746e1662f4276cf3569a24` |
