//! Crash injection (docs/spec/07-storage.md, ST-8.8 and the recovery row
//! of ST-13; Proposition 8.2): a child process (this test binary in child
//! mode) appends day batches to a hot relation, seals, consolidates and
//! appends more, and is killed at a fault point (`ABT_FAULT=<site>:<n>`
//! aborts on the n-th arrival) or by `kill -9` at a random moment. After
//! the crash the store is opened again: every answer at every durable
//! sequence equals a twin's that was never killed, no unjournaled block is
//! visible (the append files end at their last journaled block), the
//! durable batches are whole, and the script resumes from the first batch
//! the crash lost, after which the store reads as the twin does.

mod hot_common;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use abt_data::{RowFilter, Store};
use abt_wire::scalar::NANOS_PER_DAY;
use hot_common::*;

const REL: &str = "equity.close";

/// A market row: instrument, label, receipt, price.
type Row = (u64, i64, i64, f64);
const ENV_DIR: &str = "ABT_RECOVERY_DIR";

/// The script every process computes alike: the published history, the
/// day batches before the consolidation, and those after it.
fn script() -> (Vec<Row>, Vec<Vec<Row>>, Vec<Vec<Row>>) {
    let mut rng = Rng(21);
    let ids = [2u64, 3, 5, 7, 11];
    let (start, days) = (18_000i64, 3_600i64);
    let rows = market(&mut rng, &ids, start, days);
    let t_of = |d: i64| (start + d) * NANOS_PER_DAY;
    let base: Vec<_> = rows.iter().filter(|r| r.1 < t_of(1_000)).cloned().collect();
    let day_batches = |from: i64, to: i64| -> Vec<Vec<Row>> {
        let mut out = Vec::new();
        let mut d = from;
        while d < to {
            let b: Vec<_> = rows.iter().filter(|r| r.1 >= t_of(d) && r.1 < t_of(d + 10)).cloned().collect();
            if !b.is_empty() {
                out.push(b);
            }
            d += 10;
        }
        out
    };
    (base, day_batches(1_000, 2_800), day_batches(2_800, days))
}

/// The child: the script from wherever the store stands, until the end
/// or the crash. Nothing but the child runs this.
#[test]
fn child_appends_until_killed() {
    let Ok(dir) = std::env::var(ENV_DIR) else { return };
    let (base, before, after) = script();
    let mut s = Store::open(Path::new(&dir)).unwrap();
    if s.registry.current(REL, None).is_none() {
        s.ingest(REL, &[close_rows(&base)], "test", hot()).unwrap();
    }
    let v = s.registry.current(REL, None).unwrap().to_string();
    let done = batches_done(&s, &v, &before, &after);
    for b in before.iter().chain(&after).skip(done) {
        if b.len() + 1 == done + 1 {
            // (unreachable guard to keep the loop shape simple)
        }
        s.ingest(REL, &[close_rows(b)], "test", hot()).unwrap();
        if is_last_before(&before, &after, b) {
            s.consolidate(REL).unwrap();
        }
    }
    std::process::exit(0);
}

fn is_last_before(before: &[Vec<Row>], _after: &[Vec<Row>], b: &[Row]) -> bool {
    before.last().is_some_and(|l| std::ptr::eq(l.as_slice(), b))
}

/// How many batches of the script the store holds whole: the appended
/// rows past the published high-water, matched batch by batch (a batch is
/// logged whole, D-3.1).
fn batches_done(s: &Store, v: &str, before: &[Vec<Row>], after: &[Vec<Row>]) -> usize {
    let h = s.hot(v).unwrap();
    let published = h.manifest.seq_high;
    let high = h.seq_high();
    let mut appended = high - published;
    let mut done = 0;
    for b in before.iter().chain(after) {
        if appended == 0 {
            break;
        }
        assert!(appended >= b.len() as u64, "a batch is logged whole: {} rows of {} durable", appended, b.len());
        appended -= b.len() as u64;
        done += 1;
    }
    assert_eq!(appended, 0, "the durable rows are whole batches");
    done
}

/// A fresh directory with a short path.
fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("abt-recovery-{}-{}", tag, std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn spawn_child(dir: &Path, fault: Option<&str>) -> std::process::Child {
    let mut c = Command::new(std::env::current_exe().unwrap());
    c.args(["--exact", "child_appends_until_killed", "--nocapture", "--test-threads=1"])
        .env(ENV_DIR, dir)
        .env_remove("ABT_FAULT")
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Some(f) = fault {
        c.env("ABT_FAULT", f);
    }
    c.spawn().expect("the child runs")
}

/// The twin: the script up to `done` batches, in a temporary store, never
/// killed (the same rows in the same batches give the same sequence
/// numbers and the same version).
fn twin(done: usize, consolidated: bool) -> (Store, String) {
    let (base, before, after) = script();
    let mut t = Store::temporary("recovery-twin").unwrap();
    let v = t.ingest(REL, &[close_rows(&base)], "test", hot()).unwrap();
    for (i, b) in before.iter().chain(&after).take(done).enumerate() {
        t.ingest(REL, &[close_rows(b)], "test", hot()).unwrap();
        if consolidated && i + 1 == before.len() {
            t.consolidate(REL).unwrap();
        }
    }
    (t, v)
}

/// The answers of a store at the sequences `seqs` (and unbounded).
fn answers(s: &Store, v: &str, seqs: &[u64]) -> Vec<String> {
    let filters = [
        RowFilter::default(),
        RowFilter {
            identity: Some(vec![5, 11]),
            ..RowFilter::default()
        },
        RowFilter {
            t0: Some(19_500 * NANOS_PER_DAY),
            t1: Some(20_900 * NANOS_PER_DAY),
            latest_only: true,
            ..RowFilter::default()
        },
        RowFilter {
            latest_only: true,
            r_max: Some(20_700 * NANOS_PER_DAY),
            ..RowFilter::default()
        },
    ];
    let mut out = Vec::new();
    for f in &filters {
        for seq in seqs.iter().map(|s| Some(*s)).chain([None]) {
            out.push(text_with_seq(&s.read(v, &RowFilter { as_of_seq: seq, ..f.clone() }).unwrap()));
        }
    }
    out.push(text_with_seq(&[s.read_all(v).unwrap()]));
    out
}

/// After a crash: the store opened again answers as the twin does at
/// every durable sequence; the append files hold journaled blocks only;
/// then the script resumes and the final state equals a complete twin's
/// and a Parquet twin's.
fn recover_and_finish(dir: &Path, what: &str) {
    let (base, before, after) = script();
    let mut s = Store::open(dir).unwrap();
    let Some(v) = s.registry.current(REL, None).map(str::to_string) else {
        // Nothing published (the crash was in the base's staging, S5
        // never reached): the script starts over.
        assert!(
            !dir.join("hot").join(REL).exists() || std::fs::read_dir(dir.join("hot").join(REL)).unwrap().all(|e| e.unwrap().file_name().to_string_lossy().starts_with('.')),
            "{}: no version directory without a manifest",
            what
        );
        let v = s.ingest(REL, &[close_rows(&base)], "test", hot()).unwrap();
        let (_twin, tv) = twin(0, false);
        assert_eq!(v, tv);
        for (i, b) in before.iter().chain(&after).enumerate() {
            s.ingest(REL, &[close_rows(b)], "test", hot()).unwrap();
            if i + 1 == before.len() {
                s.consolidate(REL).unwrap();
            }
        }
        finish(s, &v, dir, what);
        return;
    };
    let done = batches_done(&s, &v, &before, &after);
    let consolidated = s.hot(&v).unwrap().manifest.generation > 0;
    let (t, tv) = twin(done, consolidated);
    assert_eq!(v, tv, "{}: the version id (a function of the published rows)", what);
    let high = s.hot(&v).unwrap().seq_high();
    assert_eq!(high, t.hot(&tv).unwrap().seq_high(), "{}: the durable high-water is the twin's after {} batches", what, done);
    let seqs: Vec<u64> = [high, high.saturating_sub(1), high / 2, high.saturating_sub(2_000)].into_iter().filter(|s| *s > 0).collect();
    let (ours, theirs) = (answers(&s, &v, &seqs), answers(&t, &tv, &seqs));
    for (i, (a, b)) in ours.iter().zip(&theirs).enumerate() {
        assert_same(&format!("{}: answer {} after {} batches", what, i, done), a, b);
    }
    // No unjournaled block is visible: every append file ends at its
    // journaled extent, and the journal's records are the sealed blocks.
    {
        let h = s.hot(&v).unwrap();
        let ov = h.overlay.lock().unwrap();
        for f in &ov.files {
            if f.path.exists() {
                assert_eq!(std::fs::metadata(&f.path).unwrap().len(), f.len, "{}: {} ends at its journaled extent", what, f.path.display());
            }
        }
        let sealed: usize = ov.instruments.iter().map(|i| i.sealed.len()).sum();
        assert_eq!(ov.journal_records as usize, sealed, "{}: the journal's records are the sealed blocks", what);
    }
    // The script resumes: the batches the crash lost, the consolidation
    // where the script has it, the rest.
    for (i, b) in before.iter().chain(&after).enumerate().skip(done) {
        s.ingest(REL, &[close_rows(b)], "test", hot()).unwrap();
        if i + 1 == before.len() {
            s.consolidate(REL).unwrap();
        }
    }
    if done >= before.len() && !consolidated {
        // The crash fell between the last batch before the consolidation
        // and its publication: consolidate now.
        s.consolidate(REL).unwrap();
    }
    finish(s, &v, dir, what);
}

/// The final state against a complete hot twin and a Parquet twin.
fn finish(s: Store, v: &str, dir: &Path, what: &str) {
    let (base, before, after) = script();
    let (t, tv) = twin(before.len() + after.len(), true);
    assert_eq!(v, tv);
    let high = s.hot(v).unwrap().seq_high();
    assert_eq!(high, t.hot(&tv).unwrap().seq_high(), "{}: the final high-water", what);
    let seqs = [high, high - 1, high / 2];
    for (i, (a, b)) in answers(&s, v, &seqs).iter().zip(&answers(&t, &tv, &seqs)).enumerate() {
        assert_same(&format!("{}: final answer {}", what, i), a, b);
    }
    let mut rng = Rng(22);
    let mut p = Store::temporary("recovery-parquet").unwrap();
    let mut pv = p.ingest(REL, &[close_rows(&base)], "test", BTreeMap::new()).unwrap();
    for b in before.iter().chain(&after) {
        pv = p.ingest(REL, &[close_rows(b)], "test", BTreeMap::new()).unwrap();
    }
    same_reads(&s, v, &p, &pv, &mut rng, &[2, 3, 5, 7, 11], 18_000, 3_600);
    drop(s);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn every_answer_survives_a_crash_at_every_fault_point() {
    if std::env::var(ENV_DIR).is_ok() {
        return;
    }
    let sites = [
        "publish-before-rename:1",
        "append-after-log:1",
        "append-after-log:40",
        "seal-before-record:1",
        "seal-after-record:1",
        "seal-before-record:4",
        "seal-after-record:6",
        "consolidate-before-rename:1",
    ];
    for site in sites {
        let dir = scratch(site.split(':').next().unwrap());
        let status = spawn_child(&dir, Some(site)).wait().unwrap();
        assert_eq!(
            std::os::unix::process::ExitStatusExt::signal(&status),
            Some(libc_sigabrt()),
            "{}: the child aborts at the fault point (status {:?})",
            site,
            status
        );
        recover_and_finish(&dir, site);
    }
}

#[test]
fn every_answer_survives_a_kill_at_a_random_moment() {
    if std::env::var(ENV_DIR).is_ok() {
        return;
    }
    let mut rng = Rng(23);
    for round in 0..4 {
        let dir = scratch(&format!("kill-{}", round));
        let mut child = spawn_child(&dir, None);
        let wait_ms = 60 + rng.below(900);
        let started = std::time::Instant::now();
        let mut exited = None;
        while started.elapsed().as_millis() < wait_ms as u128 {
            if let Some(st) = child.try_wait().unwrap() {
                exited = Some(st);
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        match exited {
            Some(st) => assert!(st.success(), "round {}: the child finished the script: {:?}", round, st),
            None => {
                child.kill().unwrap();
                let st = child.wait().unwrap();
                assert_eq!(std::os::unix::process::ExitStatusExt::signal(&st), Some(9), "round {}: killed", round);
            }
        }
        recover_and_finish(&dir, &format!("kill after {} ms", wait_ms));
    }
}

fn libc_sigabrt() -> i32 {
    6
}
