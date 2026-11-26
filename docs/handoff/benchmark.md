# The MW14 benchmark, as run on Nov 6

Every figure of Nov 6 (377 s, 367 s, 3.7 GB, weekly correlation 0.9777) came from the scripts below. They were written into a session's scratch directory, which has since been cleared. This document holds them as recovered from that session's log, so that package 1 of `roadmap.md` can turn them into `scripts/bench-mw14.sh`.

The log is `~/.claude/projects/-Users-alejandro-stageanalyst/19cd91e4-5b72-44ac-828a-b9d4582f7302.jsonl`. The scripts are reproduced as they ran, with only the `ssh` and `scp` wrappers removed.

**Since Nov 25 the benchmark is in the repository** (package 1 of `roadmap.md`), and this document is the record of where it came from:

| Script | What |
| --- | --- |
| `scripts/bench-mw14.sh LAKE SPXTR VIX CANON [N]` | Stages the store once (keyed by a hash of its inputs), then N cold repetitions, each on a fresh copy of the store with its own data service, each compared with the canon; one JSON line per repetition. `STORE=DIR` names a store already staged that has never served a run; the Nov 6 store at `/data/mw14-store` has (it holds that run's `kernel.<run>.*`), so the script refuses it and stages its own under `/data/bench` |
| `scripts/parity/mw14_series_bundles.py` | The series bundles of the section below |
| `scripts/parity/mw14_compare.py … --json FILE` | The comparison, also as JSON with exact counts |
| `scripts/build-profiling.sh` | The frame-pointer build, into `target/profiling` (`[profile.profiling]`) |

The machine has had the `m8-bench` role since Nov 25 and is driven through SSM Run Command. Its toolchain was installed with rustup's minimal profile: `rustfmt` and `clippy` were added that day (`rustup component add rustfmt clippy`); the bootstrap below, which is the user data the machine was launched with, has that line added. "The machine" below is the state of Nov 24: way A was taken on Nov 25. CI's toolchain is newer (1.100.0 against the machine's 1.99.1): it is installed beside the default for its clippy alone (`cargo +1.100.0 clippy`), and the benchmark's compiler does not change. The machine's `python3` has no pandas: the scripts take `PY=python3.12`, and `scripts/bench-mw14.sh` checks the imports before it stages.

## The machine

| | |
| --- | --- |
| Name, instance | `m8-bench2`, `i-01c143488531686bc` |
| Account, region | AWS profile `signos`, `us-east-1` (zone `us-east-1c`) |
| Type | `m8g.xlarge`: Graviton4, 4 vCPU, 16 GB |
| Image | `ami-0f11e0e04ccb60d0a`, Amazon Linux 2023, arm64 |
| Disk | 60 GB gp3 root volume, `vol-081d83eb3b3cee3cb`; `/data` is a directory on it |
| State | Stopped on Nov 6 at 06:04 UTC. The disk is kept |

**On Nov 24 it could not be reached** (since Nov 25 it can: way A below was taken, with the role and instance profile named `m8-bench`). It was driven over SSH with a key pair created for it (`m8-bench2`), and the private key was in the cleared scratch directory. It has no instance profile, so SSM cannot reach it either.

Two ways back. Both change AWS resources, so they are the owner's to run or approve.

**A. Give it an SSM role and start it (recommended).** This keeps the staged store and matches how the other two machines are operated. The image is the standard Amazon Linux 2023 one, which ships the SSM agent.

```
export AWS_PROFILE=signos AWS_DEFAULT_REGION=us-east-1
aws iam create-role --role-name m8-bench --assume-role-policy-document \
  '{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":{"Service":"ec2.amazonaws.com"},"Action":"sts:AssumeRole"}]}'
aws iam attach-role-policy --role-name m8-bench --policy-arn arn:aws:iam::aws:policy/AmazonSSMManagedInstanceCore
aws iam create-instance-profile --instance-profile-name m8-bench
aws iam add-role-to-instance-profile --instance-profile-name m8-bench --role-name m8-bench
aws ec2 associate-iam-instance-profile --instance-id i-01c143488531686bc --iam-instance-profile Name=m8-bench
aws ec2 start-instances --instance-ids i-01c143488531686bc
aws ssm describe-instance-information --query 'InstanceInformationList[].[InstanceId,PingStatus]' --output text
```

Do not reuse the `absolute-pipeline-machine` or `hermes-ec2-profile` profiles: they carry those machines' permissions.

**B. Start a new machine from the bootstrap below**, with the profile attached at launch. The staged store is lost and has to be rebuilt, and the lake has to reach the machine without `scp` ("Inputs" below says how).

Stop the machine when a session ends. It costs nothing stopped except its disk.

**How a code change reaches it.** The machine holds a clone and fetches from GitHub, which needs no credentials for this repository. So a change is measured after it is pushed to a work branch, and a push is the owner's to ask for. Without a push, a patch file can go the same way as the inputs.

### What should be on its disk

As left on Nov 6. Not re-checked, since the machine is stopped.

| Path | What |
| --- | --- |
| `~/abt` | A clone of `perf/m8-parallel`. Last pulled Nov 6 about 05:37 UTC, so expect the last code commit, `eb3345c`; the head `2040b40` adds only documents |
| `~/abt/target/release/abt` | The release build of that clone |
| `/data/target-prof/release/abt` | The same with frame pointers and debug info, for `perf` |
| `/data/ndlake` | The lake's `curated/bars_by_date`, `registry`, `indexes` and `classification` |
| `/data/mw14-store` | The staged store the runs read |
| `/data/series` | The two series bundles |
| `/data/canon.npy` | The canon |
| `/data/m8/mw14/run.log`, `/data/m8/run_p2.log`, `/data/m8/perf2_self.txt` | The run's output, the profiled run's output, the profile's self-time table |

## Inputs

On the Mac:

| Input | Path |
| --- | --- |
| The lake | `/Users/alejandro/market-data/NDLake` (5.6 GB) |
| `$SPXTR` daily closes | `/Users/alejandro/stageanalyst/strategy/ivmom/artifacts/spxtr_daily.parquet` |
| Weekly VIX | `/Users/alejandro/stageanalyst/strategy/ivmom/artifacts/vix_weekly.parquet` |
| The canon | `/Users/alejandro/stageanalyst/strategy/ivmom/artifacts/_canon_wkret_mw14_full.npy` |

If the machine's kept disk is intact, none of this needs copying again. On Nov 6 the files went over with `scp`. SSM has no file copy: upload to an S3 bucket in the `signos` account and fetch from the machine with a presigned URL, which needs no extra permission on the machine's role.

The lake was packed on the Mac like this. Both flags matter: without them macOS adds `._*` files, which the adapter reads as corrupt Parquet.

```
cd ~/market-data/NDLake && COPYFILE_DISABLE=1 tar czf ndlake.tgz --exclude='._*' \
  curated/bars_by_date registry indexes classification
```

## Bootstrap

The user data the machine was launched with. It installs the toolchain, clones the branch and builds.

```
#!/bin/bash
exec > /var/log/m8-boot.log 2>&1
set -x
dnf install -y git gcc perf python3.12 python3.12-pip
python3.12 -m pip install -q pandas pyarrow numpy
mkdir -p /data && chown ec2-user /data
sudo -u ec2-user bash -c '
  cd ~ && curl -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal -q
  source ~/.cargo/env
  rustup component add rustfmt clippy   # added Nov 25: the gate needs both
  git clone -q --branch perf/m8-parallel --single-branch https://github.com/KurtCoVayne/absolute-backtest.git ~/abt
  cd ~/abt && git log --oneline -1
  cargo build --release 2>&1 | tail -2
  echo BOOT-DONE
'
dnf install -y perl-open
sysctl -w kernel.perf_event_paranoid=1
```

The launch itself:

```
aws ec2 run-instances --image-id ami-0f11e0e04ccb60d0a --instance-type m8g.xlarge \
  --subnet-id subnet-07d07a8c22a8d0c20 --associate-public-ip-address \
  --block-device-mappings 'DeviceName=/dev/xvda,Ebs={VolumeSize=60,VolumeType=gp3}' \
  --user-data file://userdata.sh \
  --tag-specifications 'ResourceType=instance,Tags=[{Key=Name,Value=m8-bench2}]'
```

On Nov 6 it also passed `--key-name` and a security group that opened port 22 to one address. With SSM neither is needed; add `--iam-instance-profile Name=m8-bench` instead.

## The series bundles

The lake does not hold the two series the book reads. This script writes them as two bundles the `bundle_v2` adapter ingests. The close is cast to float32 because the book's vendor export forces that, and the weekly VIX is keyed at the week's Friday.

```python
"""Two class-`series` bundles of the first formulation's layout (bundle_v2) from the book's
own inputs: SPXTR daily closes (float32, as the book's vendor export forces) and the weekly VIX
keyed at the week's Friday, as the v1 bundle keyed its weekly rows."""
import json, sys, os, datetime as dt
import pandas as pd, pyarrow as pa, pyarrow.parquet as pq
spx, vix, out = sys.argv[1], sys.argv[2], sys.argv[3]
def write(dirname, version, rows, name):
    d = os.path.join(out, dirname); os.makedirs(os.path.join(d, "log", "value"), exist_ok=True)
    rows = rows.sort_values("T").reset_index(drop=True)
    parts = []
    for ym, g in rows.groupby(rows["T"].dt.strftime("%Y-%m")):
        p = f"log/value/{ym}.parquet"
        t = pa.table({"Name": pa.array(g["Name"], pa.string()), "T": pa.array(g["T"].values.astype("datetime64[ns]"), pa.timestamp("ns")), "V": pa.array(g["V"].astype("float64"), pa.float64())})
        pq.write_table(t, os.path.join(d, p)); parts.append(p)
    man = {"format": 2, "name": "series", "version": version, "as_of": "2026-07-31", "processing_delay_seconds": 0, "source": None,
           "created_at": dt.date.today().isoformat(), "tests": {}, "relations": [{"name": "value", "signature": "value(+Name: Label, @T: Timestamp, -V: Scalar)", "resolution": "@1d", "availability": "bar_close", "rows": int(len(rows)), "partitions": parts}]}
    json.dump(man, open(os.path.join(d, "manifest.json"), "w"), indent=1)
    print(dirname, name, len(rows), rows["T"].min().date(), rows["T"].max().date(), len(parts), "partitions")
s = pd.read_parquet(spx); s = s[s["symbol"] == "$SPXTR"]
write("series-spxtr", "mw14-spxtr", pd.DataFrame({"Name": "SPXTR", "T": pd.to_datetime(s["date"]), "V": s["close"].astype("float32").astype("float64")}), "SPXTR")
v = pd.read_parquet(vix); wk = pd.to_datetime(v["wk"])
write("series-vix", "mw14-vix", pd.DataFrame({"Name": "VIX", "T": wk + pd.Timedelta(days=4), "V": v["vix"].astype("float64")}).dropna(), "VIX (Friday-keyed)")
```

## Staging and the run

`stage4.sh`, run on the machine. It unpacks the lake, stages the store in seven chunks, serves it, runs the book once with the per-relation clock, and compares with the canon.

**Do not run it whole on the kept disk.** It deletes the staged store (`rm -rf $ST`) and rebuilds it from `/data/ndlake.tgz`, `/data/mk_series_bundles.py` and the two Parquet files, which may no longer be there. To repeat the run on the store as staged, take the script from the `SOCK=` line on, with the first line's variables.

```bash
#!/bin/bash
# The real MW14 on the pushed branch: store staged from the lake and the series bundles, the served run, the parity, the per-relation table.
source ~/.cargo/env; cd ~/abt; D=/data/m8; ST=/data/mw14-store; mkdir -p $D/mw14
say() { echo "[$(date -u +%H:%M:%S)] $*"; }
for i in $(seq 1 120); do grep -q BOOT-DONE /var/log/m8-boot.log 2>/dev/null && break; sleep 15; done; say "boot: $(tail -1 /var/log/m8-boot.log | cut -c1-80)"; git log --oneline -1 | cut -c1-80
NEW=~/abt/target/release/abt; ls -l $NEW | awk '{print $5}'
mkdir -p /data/ndlake && tar xzf /data/ndlake.tgz -C /data/ndlake && find /data/ndlake -name '._*' -delete; cd /data/ndlake && mkdir -p indexes/index_membership && mv indexes/index_membership-*.parquet indexes/index_membership/ 2>/dev/null; say "lake $(find curated/bars_by_date -type f | sort | xargs cat | sha256sum | cut -c1-16)"; cd ~/abt
python3.12 /data/mk_series_bundles.py /data/spxtr_daily.parquet /data/vix_weekly.parquet /data/series 2>&1 | tail -2
rm -rf $ST; for b in series-spxtr:1d series-vix:1w; do dir=${b%%:*}; base=${b##*:}; $NEW data ingest --store $ST --adapter bundle_v2 --from /data/series/$dir --class series --base $base > $D/mw14/ingest_$dir.log 2>&1; say "  $dir rc=$?"; done
for span in 1990-01-01:1994-12-31 1995-01-01:1999-12-31 2000-01-01:2004-12-31 2005-01-01:2009-12-31 2010-01-01:2014-12-31 2015-01-01:2019-12-31 2020-01-01:2026-12-31; do a=${span%%:*}; b=${span##*:}
  $NEW data ingest --store $ST --adapter norgate --from /data/ndlake --lists 'S&P 500=SP500;S&P MidCap 400=SP400;S&P SmallCap 600=SP600' --tickers 'SPY=ETF;QQQ=ETF' --start $a --end $b --attributes reviewed=complete > $D/mw14/ingest_$a.log 2>&1; say "  $a rc=$? $(grep '^norgate:' $D/mw14/ingest_$a.log | cut -c1-80)"; done
SOCK=$D/mw14.sock; rm -f $SOCK; /usr/bin/time -v $NEW data serve --store $ST --socket $SOCK > $D/mw14/serve.log 2>&1 & SRV=$!
for i in $(seq 1 300); do [ -S $SOCK ] && break; sleep 1; done; say "serving after ${i}s"
ABT_TIMING_RELS=1 /usr/bin/time -v $NEW run --strategy mw14 --data $SOCK --timing --history window --start 1991-01-04 --end 2026-07-02 corpus-v2/catalog corpus-v2/lib corpus-v2/company/mw14.dsl > $D/mw14/run.log 2>&1; RC=$?
say "run rc=$RC $(grep -o 'run_s=[0-9.]*' $D/mw14/run.log) front_rss_kb=$(grep 'Maximum resident' $D/mw14/run.log | grep -o '[0-9]*$') user=$(grep 'User time' $D/mw14/run.log | grep -o '[0-9.]*$')"
grep -E "^derive|^phases" $D/mw14/run.log | head -24
RUN=$(grep -o "^run [a-z0-9]*" $D/mw14/run.log | head -1 | cut -d' ' -f2)
[ -n "$RUN" ] && { say "parity"; python3.12 scripts/parity/mw14_compare.py $ST $RUN /data/canon.npy 2>&1 | grep -E '^cagr|^sharpe|^maxdd|weekly correlation' | cut -c1-200; }
kill -TERM $SRV 2>/dev/null; sleep 2; kill -KILL $SRV 2>/dev/null; grep "Maximum resident" $D/mw14/serve.log
say "STAGE4 DONE"
```

What to read in its output:

- `run_s=` is the run's time, on the line starting `timing`. The same line has `tuples=` (what the run received) and the count of rows trimmed. The lines starting `derive` and `phases` are the seconds per derived relation and per phase of the fold.
- `Maximum resident` in `run.log` is the front's peak memory; in `serve.log`, the data service's.
- The line `run <id>:` gives the run id the comparison script needs.

## The profile

The profiled run, on a build with frame pointers:

```bash
source ~/.cargo/env; cd ~/abt; D=/data/m8; ST=/data/mw14-store
CARGO_TARGET_DIR=/data/target-prof RUSTFLAGS="-C force-frame-pointers=yes -C debuginfo=2" cargo build --release --bin abt 2>&1 | tail -1; PROF=/data/target-prof/release/abt
SOCK=$D/mw14p.sock; rm -f $SOCK; ~/abt/target/release/abt data serve --store $ST --socket $SOCK > $D/serve_p.log 2>&1 & SRV=$!
for i in $(seq 1 300); do [ -S $SOCK ] && break; sleep 1; done
ABT_TIMING_RELS=1 perf record -e cpu-clock -F 299 -g --call-graph fp -o $D/perf2.data -- $PROF run --strategy mw14 --data $SOCK --timing --history window --start 1991-01-04 --end 2026-07-02 corpus-v2/catalog corpus-v2/lib corpus-v2/company/mw14.dsl > $D/run_p2.log 2>&1
perf report -i $D/perf2.data --stdio --no-children -g none --sort symbol --percent-limit 0.4 2>/dev/null | grep -v "^#" | grep -v "^$" | head -40 > $D/perf2_self.txt
perf report -i $D/perf2.data --stdio --children -g none --sort symbol --percent-limit 1.5 2>/dev/null | grep -v "^#" | grep -v "^$" | head -60 > $D/perf2_children.txt
kill -TERM $SRV 2>/dev/null; sleep 2; kill -KILL $SRV 2>/dev/null
```

The profiling scripts of issue #79, which sweep the universe size and stub one relation at a time, are in `/Users/alejandro/absolute-backtest/docs/perf/` (`run_prof.sh`, `prof_prep.py`, `prof_summarize.py`). They drive the old engine's command line and would need adapting.

## Real history only

The owner's rule of Nov 26, 2026: a benchmark or a validation run reads real market history. The synthetic 1,000-name cell this section used to hold, the synthetic stores of the panel and the lake cloned wider (`scripts/widen-lake.py`) are retired; their numbers of Nov 5 and Nov 26 stay in `docs/assessment.md` as what was measured then. A program with no real history to read (`opening_gap` and `resampled_momentum` on minute bars of equities, `pairs_trading` on two tickers no market lists) is not measured.

## The freshest data

| What | Where | To |
|---|---|---|
| Norgate lake | `s3://ndlake-094828274452` (its watermark in `_meta/manifest.json`); a copy on the machine at `/data/ndlake-20261027` | bars to 2026-10-26; the index memberships as written on Sep 19 |
| `$SPXTR` daily and the weekly VIX | `scripts/mw14-series.py LAKE BOOK_SPXTR BOOK_VIX CBOE_CSV OUT_DIR`: the book's files carried to the lake's last day, `$SPXTR` from the lake and the VIX from CBOE's history, each checked equal to the book's file where they overlap | the lake's last day |
| Databento futures, minute bars | the pipeline machine, `/srv/absolute/data/vendor/dbn*` (bucket `dbn-futures-094828274452`); the pull of Nov 26 is `dbn-refresh-20261126` | the session of 2026-11-25 |

A newer lake needs the Norgate updater on the owner's Windows machine and its upload. The lake carries `curated/bars_by_date/archive/`, bars before 1990 that repeat some rows of the `year=` partitions; nothing from 1990 on is repeated, and every stage starts there. As on Nov 6, the membership file must be moved into a directory (`indexes/index_membership/data.parquet`).

MW14 on the fresh lake is the same script with the fresh series (there is no canon for it: the canon was frozen on the June lake, which lacks the bars of 77 members of the early 1990s, so `compare` reports a difference that is the lake's):

```
python3.12 scripts/mw14-series.py /data/ndlake-20261027 /data/spxtr_daily.parquet /data/vix_weekly.parquet VIX_History.csv /data/fresh
END=2026-10-27 EXTRA="--evaluator block" scripts/bench-mw14.sh /data/ndlake-20261027 /data/fresh/spxtr_daily.parquet /data/fresh/vix_weekly.parquet /data/canon.npy 1
```

## The panel

`scripts/panel.sh NAME TREE [EXTRA...]` runs every program but MW14 once, cold, so that a change made for one book is measured on all of them. `TREE` is a directory holding a release `abt` and its `corpus-v2`. Each run reads its store through a data service of its own and holds the machine's lock for itself alone. One JSON line a run goes to `$BENCH/panel-NAME/cells.jsonl`: seconds, peak memory, decisions, fills and the SHA-256 of the returns, or why the run did not finish.

Two groups, each on a real store over its whole history:

- `eq`: every daily strategy of the corpus on the S&P 500 lake staged from 1990 (`EQ`, by default `$BENCH/pstore-eq`):

  ```
  abt data ingest --store $BENCH/pstore-eq --adapter norgate --from /data/ndlake --start 1990-01-01 --exceptions scripts/reviewed/norgate-spx-2014.parquet --attributes reviewed=complete
  ```

- `fut`: R8L as the book is written, on its 26 roots (`FUT`). The store is on the pipeline machine, where the vendor's files are, so that group runs there: `PGROUPS=fut FUT=/srv/absolute/m8/store-futures-m8-20261125 INPLACE=1 LIMIT=10800 scripts/panel.sh NAME TREE`.

A strategy that halts (a risk policy, a margin call) measures only the days before it halted; the cell says so (`halted`).

## R8L's store

`scripts/r8l-store.sh` stages the book's 26 roots from Databento's minute bars: per root, `scripts/r8l-views.py` makes one directory of links over every pull of the root (the pipeline's two generations and the newest pull, the newest winning a session both hold), then one ingest and the consolidation of the five bar relations. The options are those of absolute-pipeline's `scripts/store/point-values.sh` (sessions broken at 17:45 New York with a 20-minute gap, undefined instruments dropped, each root's point value stated) plus each root's commission per contract per side and `reviewed=complete`, which the book's `need` and `require` clauses ask for. A root is ingested once: its back-adjustment restates facts, so newer bars mean a new store.

The history is the vendor's: 2010-06-07 for CME's roots and 2018-12-23 for ICE's (KC, SB). The book asks for bars from 2000-01-01; the parity record's TradeStation history from 2000 is not in the store.

## A wider universe

The lake in `s3://ndlake-094828274452` holds the Russell indexes' own memberships (the Mac's lake holds only the three S&P lists). The Russell 3000 is the Russell 1000 and the Russell 2000 together: 12,368 names since 1990-07-03, about 3,000 at a time. MW14 runs on it by naming those lists where the book's three are expected:

```
LISTS='Russell 1000=SP500;Russell Mid Cap=SP400;Russell 2000=SP600' END=2026-10-27 EXTRA="--evaluator block" scripts/bench-mw14.sh /data/ndlake-20261027 /data/fresh/spxtr_daily.parquet /data/fresh/vix_weekly.parquet /data/canon.npy 1
```

The book's rules are unchanged (its large-cap leg reads the Russell 1000, its mid-cap leg the Russell Mid Cap, a subset of the 1000, its small-cap leg the Russell 2000), so the run measures the engine at that width and is no book of the firm's. There is no canon for it.

## Traps met on Nov 6

- **AppleDouble files.** A lake copied from macOS carries `._*` files. Pack with `COPYFILE_DISABLE=1` and `--exclude='._*'`, and delete any that remain.
- **Membership must be a directory.** The lake names its one membership file `indexes/index_membership-<timestamp>.parquet`; the adapter wants `indexes/index_membership/` holding Parquet files.
- **Start at the canon's first week.** The comparison aligns by index from the run's first bar, so the run needs `--start 1991-01-04`. Without it the correlation is meaningless.
- **End on 2026-07-02.** The book's last decision week books its cost with no return and needs the bar after it.
- **`--history all` does not finish** on 16 GB. `--history window` is the default.
- **Ingest in chunks.** The Norgate adapter holds every bar of a span in Arrow before staging; five-year spans peak at 2 to 8 GB each.
- **The series bundles are ingested without the catalog arguments**, unlike every other ingest.

## The Python side

The 25 s figure is the firm's own book, `/Users/alejandro/stageanalyst/strategy/ivmom/book.py`, timed by `mk_engine_bench.py` beside it over ten repetitions on the same machine type (issue #79). Its results are in `artifacts/engine_bench_mw14.json`. That driver also runs the old engine's three stages, so it is the model for what `scripts/bench-mw14.sh` should print.
