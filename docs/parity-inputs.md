# Parity inputs

The reference implementations of MW14 and MORNIGHT-R8L live on the research
machine (`alejandro@192.168.1.42`). Everything the parity runs read was
copied from it read-only on 2026-10-06 into `~/data/ref/` on this machine;
nothing on the research machine was changed. This page records what was
copied, from where, and the SHA-256 of each file, so that a rerun can prove it
reads the same bytes.

## MW14 (`~/data/ref/mw14/`)

| Local | Source (research machine) | What it is |
| --- | --- | --- |
| `ndlake/curated/bars_by_date/year=*/` | `~/market-data/NDLake/curated/bars_by_date` | Daily bars as traded, 1977-11 to 2026-06-24 (685 MB) |
| `ndlake/registry/corporate_actions/` | `~/market-data/NDLake/registry/corporate_actions` | SPLIT (new shares per old) and DIV rows |
| `ndlake/registry/security_master/` | `~/market-data/NDLake/registry/security_master` | SCD-2 master (not read by the book) |
| `ndlake/indexes/index_membership-*.parquet` | `~/market-data/NDLake/indexes/` | S&P 500/400/600 spells from 1990-01-02 |
| `artifacts/spxtr_daily.parquet` | `~/stageanalyst/strategy/ivmom/artifacts/` | $SPXTR daily closes (float32), the gate |
| `artifacts/vix_weekly.parquet` | same | Weekly VIX and its 4-week mean (CBOE, free) |
| `artifacts/_canon_wkret_mw14_full.npy` | same | The canonical 1,852 weekly returns (re-frozen 2026-08-14) |
| `artifacts/mw14_trades.xlsx` | same | The book's trade list (read by people, not by the scripts) |
| `code/*.py`, `code/RESULTS.md`, `code/EVOLUTION.md` | `~/stageanalyst/strategy/ivmom/` | The canonical code, read to translate the rules |

## MORNIGHT-R8L (`~/data/ref/r8l/`)

| Local | Source (research machine) | What it is |
| --- | --- | --- |
| `cache/<TICKER>.parquet` (26) | `~/d20-research/data/tradestation/cache/` | TradeStation 1-minute back-adjusted continuous contracts of the HOME-26 roots, bar-start Chicago time (1.6 GB) |
| `config/*.yaml` | `~/d20-research/config/` | `contract_specs.yaml` (point values), `costs.yaml` |
| `results/leg_flags.parquet` | `~/d20-research/results/r40/` | The golden legs (R8 HOME-26 and HOLD-6; R8L = HOME-26 main and sidecar) |
| `results/r42/` | `~/d20-research/results/r42/` | The R8L paper's exhibits: scorecard, funnel, f points, yearly, drawdowns |
| `PAPER-BOOK.md` | `~/d20-research/docs/` | The book's specification (PB-5 and its R8L amendment) |
| `code/*.py` | `~/d20-research/scripts/`, `d20research/data/` | `r31_return.py`, `r37_book.py`, `r39_fixed.py`, `r39_r8_paper.py`, `sessions.py`, `tradestation.py` |
| `commissions_per_side.json` | derived here | The commission per contract per side of each root, read off the golden legs as `comm_r x point value x ATR / 2` (constant per root to 1e-9; ES 2.297, ZN 1.72, CL 2.41, ...); `d20research/costs/ibkr.py` was not copied before the research machine became unreachable |

## Checksums (SHA-256)

Directories of many files are hashed as the concatenation of their files in
sorted path order.

```
aade0bc145059e74c05757ee4da0034009f806923ef740e25de06f0600e72013  mw14/ndlake/curated/bars_by_date (all partitions)
e5fb5a4ecac445b0bd2b68aa8e1095baca13e8e31f101432c03e3886c0846bbf  r8l/cache (26 files)
712a2184d997864a6f39958bf5f9993d61a2b7f14c23d251dbde770f4b0e034f  mw14/artifacts/_canon_wkret_mw14_full.npy
fdc2cc1e2a8fcbb8acf80af66a8cc4b5eac0c3caba90a79ac1e75d93579be315  mw14/artifacts/spxtr_daily.parquet
f9a744cac740d302cfcc230fa9b0dc16fe2df6a618c9023341204625aa78777f  mw14/artifacts/vix_weekly.parquet
a930ce08a1c25c6113313c17c6ce6267b32ab5cf94262f9dfef86904b21d35bb  mw14/code/book.py
34578dec68250d5c1a7959b3c53ac738f3195313842df804f7231df552aeaa93  mw14/code/data.py
a8796055fb3926cb1094575cd52f86b71fd428a509c55477535fa2bfd69e7fa1  mw14/code/engine.py
03a9e5947c7dd450758fa97f203caa2eafa4ff716b1b450a27ee10906e39be54  mw14/code/engine_weighted.py
199345f763ac13c8e7b586e5efd7841234d52d09b51e78111cd98b9f86d181ab  mw14/code/run_mw1_core.py
9671789a4fd867557d04121a8718e1023a3c29dc81cbaa237d7310cf1ee007ac  mw14/code/run_mw3_iv.py
43cbe69bc426ed923c91bdb65c287c6d79d7d0cd9156964f3be161a3dda442fd  mw14/code/vix.py
c1c07ddc6d5ab8c4e454ffa50e5a95e121951a0d269cf34ddfbf22957ee25836  mw14/ndlake/indexes/index_membership-20260625T160037074799Z.parquet
87e4f0aca424767d3ece13d513726994be51450cd1399d3f369f749d75a0c3d7  mw14/ndlake/registry/corporate_actions/part-20260625T160226619740Z.parquet
c832e7192808d8f31b933f744dfa7b0aab260dc9d2b8bd14bd6fad9f17cd3433  mw14/ndlake/registry/security_master/part-20260625T155619113606Z.parquet
54b2d67928bd95f68df4f13fff3bcbb540ddd8db97e52444a418e2adbe4706a8  r8l/code/r31_return.py
a03ad2f66bd7380528a7db8ed9c3ed883a6987a61d5dfc475f98c88d32394de1  r8l/code/r37_book.py
f8a109b084d0ff4fe53c2bf1ebaf844e5c1c950efd69881b4e155c6ce7600f0a  r8l/code/r39_fixed.py
c4f47d801e9981dc4c2c2ab252d6670de817b0340055523add71d8f44b145816  r8l/code/r39_r8_paper.py
3317d315a53c591619f435e799195cd55110d5c18406868a09b69ea1f1301fcf  r8l/code/r40_cleanroom.py
4fa1e9abee8a19a263ca5e7c65ae1f497b4352e9ad892b1d92beb0de5c5a3b31  r8l/code/sessions.py
f360a8ef8b0e1e4978c186fa49339666e83c82f3cabe9fdd6b3de5792001e2ac  r8l/code/tradestation.py
7023a7c452d7ff806d8786539a5979758b932dbf55b8134e299909da89bcf3b6  r8l/commissions_per_side.json
c45492ddaa3318cd2d36cec99b687eb046a1886e9fc035082178cb1e7159ba7d  r8l/config/contract_specs.yaml
642eb40e5a05d2e289f69cc1125a71c6513efc70879cfac66ea0dcad89cbb89c  r8l/config/costs.yaml
b78616bb2de955f3a687047398710708f750218a173fd14770fc987b3239be65  r8l/config/databento_new_specs.yaml
dfab3e77587fdd3a01c7f02dac9193f1d94f52ed1a95541ceee7afe0e459da0c  r8l/config/splits.yaml
b225f47d8e4e67a99da046ad42d0928b51665761be484bb0b043f5ed83ce7905  r8l/config/tradestation_specs.yaml
9829ea8ab1777099c5056be94fcab409d48a678578452184cd180fd23d99079f  r8l/config/universe.yaml
2bf5fde4cb2d36bc3ce3424604435356b6e42ecf85d653910a2d4a77749a73ca  r8l/results/leg_flags.parquet
```
