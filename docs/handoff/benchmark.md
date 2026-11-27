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
| Norgate lake | `s3://ndlake-094828274452` (its watermark in `_meta/manifest.json`); a copy on the machine at `/data/ndlake-20261027` | bars to 2026-10-26, and 2,694 rows of Oct 27, the day it was uploaded on; the index memberships as written on Sep 19 |
| `$SPXTR` daily and the weekly VIX | `scripts/mw14-series.py LAKE BOOK_SPXTR BOOK_VIX CBOE_CSV OUT_DIR`: the book's files carried to the lake's last day, `$SPXTR` from the lake and the VIX from CBOE's history, each checked equal to the book's file where they overlap | the lake's last day |
| Databento futures, minute bars | the pipeline machine, `/srv/absolute/data/vendor/dbn*` (bucket `dbn-futures-094828274452`); the pull of Nov 26 is `dbn-refresh-20261126`, with the session days it had cut repaired in place and the session of Nov 26 added on Nov 27; `dbn-repair-20261126` holds the days the vendor has more of than any pull, and `dbn-reframe-20261127` the stretches a pull framed off their day ("R8L's store", "What the pulls lacked") | the session of 2026-11-26, Thanksgiving, on which 16 of R8L's 26 roots traded (the grains, the livestock, KC and SB did not) |

A newer lake needs the Norgate updater on the owner's Windows machine and its upload. On Nov 27 there is none to read: the bucket's manifest and its 2026 file are of Oct 27 11:31, the pipeline machine holds that lake and one of Oct 15, the Mac's `~/market-data/NDLake` is of Jun 25, and the account's three machines run Linux. The vendor's futures are asked for where its range ends (`scripts/r8l-repair.py range`: both datasets to the minute, the same morning). The lake carries `curated/bars_by_date/archive/`, bars before 1990 that repeat some rows of the `year=` partitions; nothing from 1990 on is repeated, and every stage starts there. As on Nov 6, the membership file must be moved into a directory (`indexes/index_membership/data.parquet`).

MW14 on the fresh lake is the same script with the fresh series (there is no canon for it: the canon was frozen on the June lake, which lacks the bars of 77 members of the early 1990s, so `compare` reports a difference that is the lake's):

```
python3.12 scripts/mw14-series.py /data/ndlake-20261027 /data/spxtr_daily.parquet /data/vix_weekly.parquet VIX_History.csv /data/fresh
END=2026-10-27 EXTRA="--evaluator block" scripts/bench-mw14.sh /data/ndlake-20261027 /data/fresh/spxtr_daily.parquet /data/fresh/vix_weekly.parquet /data/canon.npy 1
```

A staging ends with every hot relation consolidated with its tails (`abt data consolidate RELATION --tails`, E-173; `scripts/bench-mw14.sh` does it after the last span). The store's rows are then all in its base segments: a service opens it without reading its log, and its start in the benchmark falls from 32.6 s to 9.0 s, the time to read the segments from a cold disk. A store staged in spans by an engine before E-137 is healed by the same command with an engine after it.

The staged store is kept under `$BENCH/store-<key>`, the key a hash of the lake, the series, the spans, the lists and the data layer's source at the tree's commit. A tree with no `.git` (a snapshot under `/data/bench/trees`) has no commit to hash, so a store staged by an older engine is reused: pass `RESTAGE=1` after a change to `abt-data` or `abt-codec`.

## The panel

`scripts/panel.sh NAME TREE [EXTRA...]` runs every program but MW14 once, cold, so that a change made for one book is measured on all of them. `TREE` is a directory holding a release `abt` and its `corpus-v2`. Each run reads its store through a data service of its own and holds the machine's lock for itself alone. One JSON line a run goes to `$BENCH/panel-NAME/cells.jsonl`: seconds, peak memory, decisions, fills and the SHA-256 of the returns, or why the run did not finish.

Two groups, each on a real store over its whole history:

- `eq`: every daily strategy of the corpus on the S&P 500 lake staged from 1990 (`EQ`, by default `$BENCH/pstore-eq`):

  ```
  abt data ingest --store $BENCH/pstore-eq --adapter norgate --from /data/ndlake --start 1990-01-01 --exceptions scripts/reviewed/norgate-spx-2014.parquet --attributes reviewed=complete
  ```

- `fut`: R8L as the book is written, on its 26 roots staged with each root's primary session (`FUT`; "R8L's store", below). The store is on the pipeline machine, where the vendor's files are, so that group runs there, to the store's last whole session: `PGROUPS=fut FUT=STORE INPLACE=1 LIMIT=10800 scripts/panel.sh NAME TREE --end 2026-11-27`.

A strategy that halts (a risk policy, a margin call) measures only the days before it halted; the cell says so (`halted`).

## R8L's store

`scripts/r8l-store.sh` stages the book's 26 roots from Databento's minute bars: per root, `scripts/r8l-views.py` makes one directory of links over every pull of the root (the pipeline's two generations, the newest pull, the repair's days and the stretches written again whole; where two hold a day, the file with more rows, and such a stretch from its own pull alone), then one ingest, the consolidation of the five bar relations with their tails (E-173), so that the store is whole in its base segments after every root and the next root's consolidation reads no log, and the eviction of the versions the ingest superseded (`abt data gc --disk-budget 0`). The options are those of absolute-pipeline's `scripts/store/point-values.sh` (undefined instruments dropped, each root's point value stated) plus each root's commission per contract per side and `reviewed=complete`, which the book's `need` and `require` clauses ask for, and each root's primary session (`SESSIONS=primary`, the default: `--session-windows`, E-176; the windows are those of `d20-research`'s `d20research/data/sessions.py`, written in New York time with the day each later one starts). `SESSIONS=day` leaves the adapter's own sessions, the electronic day ended where a gap of more than 20 minutes holds the 17:45 New York break, which is what the pipeline's stores hold. A root is ingested once: its back-adjustment restates facts, so newer bars mean a new store.

The history is the vendor's: 2010-06-07 for CME's roots and 2018-12-23 for ICE's (KC, SB). The book asks for bars from 2000-01-01; the parity record's TradeStation history from 2000 is not in the store.

The store, staged on Nov 27 from the pulls as repaired (below), is on the pipeline machine's disk at `/srv/absolute/m8/store-r8l-primary-20261126`: 26 roots, 33,249,167 minute bars on the continuous series from 2010-06-07 to the session of 2026-11-26, 3.1 GB (log 2.36, part 0.32, hot 0.37), staged in 25 minutes (the ingests 909 s, the consolidations 496 s, the evictions 56 s; CL's ingest peaks at 10.4 GB). ES and NQ have 4,234 sessions, the treasuries 4,247, RTY 2,421, KC and SB 1,992, ETH 1,551, and all 26 roots have Sep 1 and Oct 1, 2026. Its views are `/srv/absolute/m8/views-20261126`. `scripts/r8l-store.sh` with no setting but its places stages it again (`M=/srv/absolute/m8 B=$M/target2/release/abt-data STORE=... VIEWS=... scripts/r8l-store.sh`). The room for it came from the whole-day store beside it, by the engine's own eviction: `abt data gc --disk-budget 0` on that store took 6.5 minutes and 1.3 GB, its part directory went from 13.5 GB to 2.8 GB and the store from 20.9 GB to 10.0 GB. Two earlier stagings are not this one. `store-r8l-primary-20261125`, still on that disk, was made before any repair (33,217,867 bars; of the 252,756,874 bars read, 120,233,517 lie outside the windows and are dropped): it lacks the primary session of 2026-09-01 and 2026-10-01 for 16 roots. The one of Nov 26 (33,230,400 bars, 22.5 minutes in a tmpfs, to the session of Nov 25) was made for one run and not kept. The stated commissions are the book's own (`d20-research`, `config/costs.yaml`), which `futures_fixed_base` reads (`commission per_contract from catalog`). By E-71 a program that writes `commission per_contract 3.75` (`futures_house`, the pipeline's agents) pays the catalog's rate on this store and not the one it writes: the owner's call left open in E-96, met by the agents on Nov 26 (`docs/assessment.md`).

The whole-day store of the same pulls (`store-futures-m8-20261125`, what `SESSIONS=day GC=0` stages) holds 96,308,743 bars a relation and is 19 GB (log 6.2, part 11.6, hot 1.0). Of its part directory 8.4 GB is `continuous`, most of it versions a later root's ingest superseded: each ingest writes the constant relations and the continuous series' map again, and `gc` evicts them (done on Nov 27, above). It was staged from the pulls before their repair. R8L on that store is another rule (`session + 30min` is half an hour after the evening open): it halted at 2014-06-24 with its base lost, and none of it is R8L's record. The pipeline's own strategies are written for the electronic day, so the two are two stores.

**What the pulls lacked (Nov 26 and 27).** Four things, each found by a different check, all repaired from the vendor through the pulls' own cost gate into pulls of this work's own (`dbn-repair-20261126`, `dbn-reframe-20261127`); the pipeline's files are as they were.

*Session days cut at a month's start.* absolute-pipeline's month-chunked pull (`dbn_pull_extra.py`, one month a request) writes a session day's file from the first chunk that holds any of it and skips that day in the next chunk. A session that opens before midnight UTC on a month's last day keeps its first two hours and loses the rest. In the pull of Nov 26 that was 2026-09-01 and 2026-10-01: 116 day files in 59 of its 81 roots, 18 of R8L's 26 among them, and the view took the newest pull's file where `dbn-live` held 2026-09-01 whole. The store staged from it had no primary session on those two days for 16 roots (the equity indices, the yen, the treasuries, the energies, the metals and ETH), found when R8L's report came up $34,478 short of its fills on days with no ES session. `scripts/r8l-repair.py boundary 2026-09-01 2026-10-01` asked the vendor for the two UTC days around each through the pulls' own cost gate and replaced the 116 files with the whole day; the view now takes the file with more rows where two pulls hold a day; all 26 roots have their session on both days.

*Days some roots lack.* The same question put for the other weekdays on which some roots trade and others have no session (`r8l-repair.py holes`, 15 days of 26 roots): 2020-02-28 was missing or cut at 15:58 UTC in the history for 22 roots and the vendor holds more of it (`dbn-repair-20261126`, 22 files, 1.1 MB), whole for the equity indices, the yen and the livestock and with no bar between 14:00 and 20:00 UTC for the treasuries, the metals, the energies and the grains, so those have a session of 40 minutes (the treasuries, the metals) or none that day. On 2012-02-06 to 02-10 (GF, HE, LE), 2012-09-12 (GC), 2014-06-11 to 06-13, 2014-09-23 to 09-25, 2014-10-03 and 2014-12-31 the vendor returns no more than the files hold: its own gaps. The store states `reviewed=complete` all the same; no step of the staging counts a session's bars against its window.

*Days framed off their day (Nov 27).* The pulls' writer drops the vendor's duplicate bars and then groups the frame by a day series numbered from zero (`d20-research`, `d20research/data/ingest.py:80`, and absolute-pipeline's `scripts/store/dbn_pull_extra.py:102`: `df.groupby(session_day(df["ts_event"], dataset))` after `drop_duplicates`). pandas aligns the two by row label, so every row after a duplicate takes the day of the row as many places later as duplicates were dropped before it. From a chunk's first duplicate on, each day file holds the end of the day before and lacks its own end, and the chunk's last rows, as many as its duplicates, are written nowhere: six bars over two days with one sent twice come out as two files of five bars, the first day's last bar in the second day's file. `scripts/r8l-audit.py` reads it off the footers, exactly: a file's first and last bar must belong to the day it is named for by the pulls' own rule. None of the 24 CME roots has such a file. KC and SB have them from 2026-06-22: 39 files each in `dbn-ifus` and 78 of 82 in the pull of Nov 26, whose chunks are months, so the shift starts again each month and reaches about 1,500 rows by its end. Of KC's 303,233 bars from Aug 3 to Nov 25 that pull lacked 6,195: the last hours of Aug 31, Sep 30, Oct 30 and Nov 25 were in no file. `scripts/r8l-repair.py span` asked for both roots from 2026-06-01 again, a row's day taken by position (125 days each, 490,188 and 657,935 bars), and the view takes that stretch from `dbn-reframe-20261127` alone, since the other pulls' files there hold another day's bars.

*What the vendor counts (Nov 27).* `scripts/r8l-repair.py count` holds each root's rows in the pulls, a year at a time, against the vendor's own record count for the same session days, and halves a year that differs down to its days. For the 24 CME roots every year agrees except on 17 days, 102 root-days: 2018-12-05 and 2025-01-09 (the two national days of mourning: no pull has a file of them for 18 and 23 roots, and the vendor holds 900 to 27,000 bars a root), 2010-06-07 (the dataset's first evening, 17 roots, and two more below), 2020-02-27 (16 roots, 3 to 4% short), 2018-08-05 (a Sunday of 1 to 7 bars, 14 roots), ETH's weekends of May and June 2026, and five single days. 100 are written whole; GF and LE on 2026-11-26 are counted (1,045 each) and no bar is returned. Before it the footers' second check (a day short beside the same weekday's neighbours, then the vendor asked: 945 root-days) had found 2020-06-30 cut at 14:10 UTC for six roots and the first evening short for HO and NG: 108 root-days in all. For KC and SB the count does not compare: it runs about 15% over the bars the vendor returns for the day (3,263 counted and 2,782 returned for KC on 2019-01-16); 79 of their days before June 2026 asked outright equal the files.

*The pipeline's own pulls* carry the first and the third fault and are not changed here; its stores were staged from the files as they were. The days are written whole beside them, for a restaging through `scripts/r8l-views.py`. Its ICE roots are in `dbn-reframe-20261127`: CC and CT from 2026-06-01 (125 and 149 days), and DX, MFS, MME, OJ and RS from 2018-12-23 (1,993 to 2,471 days each), since nearly every file of those five is framed off its day from the dataset's start (2,425 of DX's 2,437 in `dbn-extra`). The first days of a month with the cut's signature (a file whose last bar is before its day's midnight UTC, beside a next day of four times its rows) are 371 in 19 of its other roots, 273 of them in `dbn-extra` (EMD on 2024-02-01: 25 rows; `dbn-live`'s 6S on 2025-04-01: 69), and were asked for one by one into `dbn-repair-20261126`: 262 written whole, and 109 on which the vendor holds no more than the file (holidays). The fix in the writer is one line, a row's day taken by position (`.to_numpy()`, or the frame's index reset after the duplicates are dropped), and a day written from every chunk that holds part of it.

**R8L on it** (Nov 27, with E-177 and E-178): `r8l.dsl` as the book is written, from the store's first bar to the session of 2026-11-26. 462.5 s, 2,484,733 barriers, 16,353 legs (32,706 fills, none dropped), the kernel at 8.1 GB and the data service at 5.5 GB. A barrier costs 0.14 ms on average (p99 3.0 ms, max 21.4 ms) and a decision barrier 5.2 ms (p99 13.3 ms, max 21.4 ms). The run's own report, on ES's 4,234 sessions from 2010-06-07 at the fixed base of $1M: annual return 20.88% (additive), Sharpe 1.302, max drawdown 22.90% (additive), profit factor 1.297 a period and 1.178 a trade, 52.7% of the 16,353 trades won, total P&L $3,507,482. (On the staging of Nov 26, in memory and to the session of Nov 25: 441.7 s, 16,348 legs, Sharpe 1.301 on 4,233 sessions. Before any repair: 439.0 s, 16,347 legs, Sharpe 1.310 on 4,231 sessions.)

Beside the Python book's legs (`scripts/parity/r8l_legs.py STORE RUN LEGS_CSV`): the book's bars are TradeStation's from 2000 and the store's are Databento's from 2010, so this compares two vendors' histories and is not the parity of `docs/parity-r8l.md`. Over the days both cover, 2011-05-25 to 2026-08-14 (each root from its 251st session in the store), the book makes 15,555 legs and abt 15,493; 14,749 are the same market and day (94.8% of the book's), 99.92% of those in the same direction, with a leg P&L correlation of 0.973. On ES's 3,910 session days there, legs summed by day over the book's fixed base:

| | the book | abt |
|---|---|---|
| annual return | 22.35% | 21.31% |
| Sharpe | 1.365 | 1.326 |
| max drawdown | 19.17% | 17.67% |
| total | 346.7% | 330.6% |
| profit factor | 1.305 | 1.297 |

The two daily series correlate 0.964. The book's column moved with the first repair: 2020-02-28 became a day both cover for seven more roots, the treasuries and the metals, where the book makes $38,693 on the whole session and abt trades the vendor's 40 minutes of it (open item 5 of `roadmap.md`, package 6). Before the repairs the table read 22.07% and 21.33%, 1.347 and 1.329, with a correlation of 0.965; after the first, 21.22% and 1.321. By root, the share of the book's legs that abt makes runs from 79% (SB), 80% (KC) and 84% (HE) to 99% (GC, NQ, RTY).

Three things are open on it:

- **The kernel evaluates the session bars itself** for roots whose hours differ (E-177). The run that read the pass's bars took 379.3 s and 1.0 ms a decision barrier, and its bars were cut at other roots' closes: it made 19,599 legs, 87.6% of the book's and 5,001 the book does not make, at a P&L per matched leg 1.45 to 1.5 times the book's. The pass needs a list of labels per instrument before it can answer them again (`roadmap.md`, package 6).
- **A store refuses a run decided otherwise.** A run's outputs are keyed by its id, which is made of the program, the binding and the window and not of the engine's version. The store of Nov 25 on the pipeline machine's disk holds the outputs of three runs by three engines (before E-177 with `--end 2026-11-25`, before E-178 with `--end 2026-11-26`, after it with `--end 2026-11-27`), so a later tree's run of the first two windows is refused on it (`two rows of one identity at one time and receipt time differ`). The store of Nov 27 holds one run, `d20d1343…`, made with `--end 2026-11-27`.
- **The report's total is its calendar's days'.** A report on a calendar counts the NAV change on the calendar's days and only those (`metrics::period_returns_on`). The run's P&L is $3,537,607 (the fills' cash flows, and the final cash less the base) and $3,507,482 of it falls on ES's 4,234 session days; the other $30,124 falls on 14 days that have no ES session: five Good Fridays on which the treasuries and the yen trade, Oct 29 and 30, 2012, four days of 2014 on which the vendor has no ES bar (06-12, 06-13, 09-25, 12-31), the two national days of mourning (2018-12-05, 2025-01-09), on which the equity indices stop before their primary session, and one Sunday of 2026, on which ETH trades.

Until E-178 the run's own report had no periods on a vendor store: `sessions of "ES"` named the symbol, and the root's series there is `ES.v.0`.

**`abt data gc` on real data.** On a copy of the folded MW14 store of the fresh lake, `abt data gc --store DIR --disk-budget 0` evicted 58 superseded versions in 9.2 s (76 version manifests to 18), and MW14 on it gave the same returns (`2fc850cad917…`) in 65.2 s.

## A wider universe

The lake in `s3://ndlake-094828274452` holds the Russell indexes' own memberships (the Mac's lake holds only the three S&P lists). The Russell 3000 is the Russell 1000 and the Russell 2000 together: 12,368 names since 1990-07-03, about 3,000 at a time. MW14 runs on it by naming those lists where the book's three are expected:

```
SPANS="1990-01-01:1991-12-31 1992-01-01:1993-12-31 ... 2026-01-01:2026-12-31" \
LISTS='Russell 1000=SP500;Russell Mid Cap=SP400;Russell 2000=SP600' END=2026-10-27 EXTRA="--evaluator block" scripts/bench-mw14.sh /data/ndlake-20261027 /data/fresh/spxtr_daily.parquet /data/fresh/vix_weekly.parquet /data/canon.npy 1
```

The book's rules are unchanged (its large-cap leg reads the Russell 1000, its mid-cap leg the Russell Mid Cap, a subset of the 1000, its small-cap leg the Russell 2000), so the run measures the engine at that width and is no book of the firm's. There is no canon for it.

Measured on Nov 26 at `3c10fc5` on `m8-bench2` (4 CPUs, 15 GB), the store staged in 19 spans of two years: 12,373 assets from 1990 to 2026-10-26, a 3.7 GB store staged in 46 minutes, and **the run in 164.8 s**, under the owner's bound of 3 minutes. 5,115 decisions; the run's process at 2.57 GB and the data service at 8.54 GB, ready after 18.0 s; 129,628,985 rows trimmed. The stream is 135.2 s of it (decide 63.6, fill 23.0, insert 20.0, decode 7.3, trim 6.8, `inv` 5.6), the checks 25.0 s and the pushed aggregates 17.6 s. Its weekly returns (`d75e475ba90a…`) correlate 0.752 with the S&P 1500 canon, which is no reference for it.

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
