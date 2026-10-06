//! The bundle format (data-bundle doc, section 2 "Storage", section 3
//! "Bundle tests"): a versioned directory the system ships. `manifest.json`
//! names the environment, the version, the bundle date and every relation
//! with its availability convention and partitions; `securities.csv` is the
//! security table; `log/<relation>/<YYYY-MM>.parquet` are append-only
//! partitions of the primitive facts by the month of their temporal key;
//! `snapshots/` is reserved for the fold kernel's checkpoints. A bundle whose
//! tests have not passed is not run.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use arrow::array::{Array, ArrayRef, Float64Array, Int64Array, StringArray};
use arrow::datatypes::{DataType, Field, Schema};
use arrow::record_batch::RecordBatch;
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use parquet::arrow::ArrowWriter;
use serde::{Deserialize, Serialize};

use crate::check::Program;
use crate::data::check_identities;
use crate::ir::*;
use crate::kernel::time::{bucket, format_timestamp, parse_timestamp};
use crate::kernel::{Dataset, Value};

/// One relation of the bundle.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct RelationEntry {
    pub name: String,
    /// The signature as declared, for the reader.
    pub signature: String,
    pub resolution: String,
    /// When a tuple is available: `bar_close` for every v1 relation (the
    /// fold kernel's availability column, M3, materialises it).
    pub availability: String,
    pub rows: usize,
    /// Partition paths relative to the bundle directory.
    pub partitions: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct TestResult {
    pub name: String,
    pub passed: bool,
    pub detail: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct TestReport {
    /// When every test passed (RFC 3339 date); absent while any fails.
    pub passed_at: String,
    pub results: Vec<TestResult>,
}

/// A problem a person reviewed and accepted (section 3, bundle tests): the
/// test, the security id, the bar, and why the data is right and the test
/// too strict there. Read from `exceptions.csv` next to the manifest
/// (`test,security,date,reason`); an accepted problem passes the test and
/// is counted in its detail.
#[derive(Clone, Debug, PartialEq)]
pub struct Exception {
    pub test: String,
    pub security: String,
    pub t: i64,
    pub reason: String,
}

/// The reviewed exceptions of a bundle; none without the file.
pub fn read_exceptions(dir: &Path) -> Result<Vec<Exception>, String> {
    let path = dir.join("exceptions.csv");
    if !path.exists() {
        return Ok(vec![]);
    }
    let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {}", path.display(), e))?;
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate().skip(1).filter(|(_, l)| !l.trim().is_empty()) {
        let fields: Vec<&str> = line.splitn(4, ',').map(|f| f.trim()).collect();
        let [test, security, date, reason] = fields[..] else {
            return Err(format!("{}:{}: expected test,security,date,reason", path.display(), i + 1));
        };
        let t = crate::kernel::time::parse_timestamp(date).ok_or_else(|| format!("{}:{}: `{}` is not a date", path.display(), i + 1, date))?;
        if reason.is_empty() {
            return Err(format!("{}:{}: an exception needs a reason", path.display(), i + 1));
        }
        out.push(Exception {
            test: test.to_string(),
            security: security.to_string(),
            t,
            reason: reason.to_string(),
        });
    }
    Ok(out)
}

/// Write the reviewed exceptions next to the manifest (none: no file).
pub fn write_exceptions(dir: &Path, exceptions: &[Exception]) -> Result<(), String> {
    if exceptions.is_empty() {
        return Ok(());
    }
    let mut text = String::from("test,security,date,reason\n");
    for e in exceptions {
        if e.test.contains(',') || e.security.contains(',') {
            return Err(format!("exception {:?}: a test or security id with a comma", e));
        }
        text.push_str(&format!("{},{},{},{}\n", e.test, e.security, format_timestamp(e.t), e.reason.replace('\n', " ")));
    }
    let path = dir.join("exceptions.csv");
    std::fs::write(&path, text).map_err(|e| format!("{}: {}", path.display(), e))
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Manifest {
    /// The environment the bundle instantiates.
    pub name: String,
    pub version: String,
    /// The bundle date (YYYY-MM-DD): what a ticker literal resolves at.
    pub as_of: Option<String>,
    /// Section 10 item 4: the offset between a bar's close and the moment
    /// its data could be acted on; zero records the bar-close convention.
    pub processing_delay_seconds: i64,
    /// Where the data came from and the schema decisions taken on the way
    /// (section 10: consolidated bars, the availability offset).
    #[serde(default)]
    pub source: Option<String>,
    pub created_at: String,
    pub relations: Vec<RelationEntry>,
    pub tests: Option<TestReport>,
}

fn today() -> String {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
    format_timestamp(secs - secs.rem_euclid(86_400))
}

fn arrow_type(ty: &Ty) -> Result<DataType, String> {
    Ok(match ty {
        Ty::Equity | Ty::Label => DataType::Utf8,
        Ty::Timestamp | Ty::Count => DataType::Int64,
        Ty::Quantity(_) => DataType::Float64,
        t => return Err(format!("a {} column cannot be stored in a bundle", t)),
    })
}

fn schema_of(sig: &Signature, with_availability: bool) -> Result<Schema, String> {
    let mut fields: Vec<Field> = sig.args.iter().map(|a| Ok(Field::new(a.name.clone(), arrow_type(&a.ty)?, false))).collect::<Result<_, String>>()?;
    if with_availability {
        fields.push(Field::new("available_at", DataType::Int64, false));
    }
    Ok(Schema::new(fields))
}

/// `name(+A: Equity, @T: Timestamp, -P: Price<USD>) @1d [complete]`.
fn signature_text(sig: &Signature) -> String {
    let args: Vec<String> = sig
        .args
        .iter()
        .map(|a| {
            let mode = match a.mode {
                Mode::In => "+",
                Mode::Out => "-",
                Mode::Key => "@",
            };
            format!("{}{}: {}", mode, a.name, a.ty)
        })
        .collect();
    format!(
        "{}({}){}{}",
        sig.name,
        args.join(", "),
        sig.res.map(|r| format!(" @{}", r)).unwrap_or_default(),
        if sig.complete { " complete" } else { "" }
    )
}

fn month_label(t: i64) -> String {
    format_timestamp(t)[..7].to_string()
}

/// Write `ds` as a bundle of environment `name` at `version` into `dir`
/// (created or overwritten), untested.
pub fn write_bundle(prog: &Program, ds: &Dataset, dir: &Path, name: &str, version: &str) -> Result<Manifest, String> {
    if name != prog.environment {
        return Err(format!("the program is written against `{}`, not `{}`", prog.environment, name));
    }
    fs::create_dir_all(dir.join("log")).map_err(|e| format!("{}: {}", dir.display(), e))?;
    if !ds.securities.is_empty() {
        let mut out = String::from("id,ticker,from,to\n");
        for s in &ds.securities {
            out.push_str(&format!(
                "{},{},{},{}\n",
                ds.symbols.name(s.id),
                s.ticker,
                format_timestamp(s.from),
                s.to.map(format_timestamp).unwrap_or_default()
            ));
        }
        fs::write(dir.join("securities.csv"), out).map_err(|e| e.to_string())?;
    }
    let mut relations = Vec::new();
    for (rel, sig) in &prog.relations {
        if !matches!(sig.kind, Kind::Primitive { .. }) || rel == "ticker" {
            continue;
        }
        let Some(tuples) = ds.facts.get(rel) else { continue };
        let key_pos = sig.key_pos().ok_or_else(|| format!("`{}` has no temporal key", rel))?;
        let res = sig.res.ok_or_else(|| format!("`{}` has no resolution", rel))?;
        let records = ds.availability_of(rel).is_some();
        let schema = Arc::new(schema_of(sig, records)?);
        let mut by_month: BTreeMap<String, Vec<(usize, &Vec<Value>)>> = BTreeMap::new();
        for (i, tu) in tuples.iter().enumerate() {
            let t = tu[key_pos].as_time().ok_or_else(|| format!("`{}`: a tuple without a timestamp key", rel))?;
            by_month.entry(month_label(t)).or_default().push((i, tu));
        }
        let rel_dir = dir.join("log").join(rel);
        fs::create_dir_all(&rel_dir).map_err(|e| e.to_string())?;
        let mut partitions = Vec::new();
        for (month, rows) in by_month {
            let mut columns: Vec<ArrayRef> = Vec::new();
            for (i, arg) in sig.args.iter().enumerate() {
                let col: ArrayRef = match &arg.ty {
                    Ty::Equity => Arc::new(StringArray::from(rows.iter().map(|(_, tu)| tu[i].as_equity().map(|s| ds.symbols.name(s))).collect::<Vec<_>>())),
                    Ty::Label => Arc::new(StringArray::from(rows.iter().map(|(_, tu)| ds.label_name(&tu[i])).collect::<Vec<_>>())),
                    Ty::Timestamp => Arc::new(Int64Array::from(rows.iter().map(|(_, tu)| tu[i].as_time()).collect::<Vec<_>>())),
                    Ty::Count => Arc::new(Int64Array::from(
                        rows.iter().map(|(_, tu)| if let Value::Count(c) = &tu[i] { Some(*c) } else { None }).collect::<Vec<_>>(),
                    )),
                    Ty::Quantity(_) => Arc::new(Float64Array::from(rows.iter().map(|(_, tu)| tu[i].as_f64()).collect::<Vec<_>>())),
                    t => return Err(format!("`{}`: a {} column cannot be stored", rel, t)),
                };
                if col.null_count() > 0 {
                    return Err(format!("`{}`: a value of column `{}` is not of its declared type", rel, arg.name));
                }
                columns.push(col);
            }
            if records {
                let avail: Vec<i64> = rows.iter().map(|(i, tu)| ds.available(rel, *i, tu[key_pos].as_time().unwrap_or(0), res)).collect();
                columns.push(Arc::new(Int64Array::from(avail)));
            }
            let batch = RecordBatch::try_new(schema.clone(), columns).map_err(|e| e.to_string())?;
            let path = rel_dir.join(format!("{}.parquet", month));
            let file = File::create(&path).map_err(|e| format!("{}: {}", path.display(), e))?;
            let mut writer = ArrowWriter::try_new(file, schema.clone(), None).map_err(|e| e.to_string())?;
            writer.write(&batch).map_err(|e| e.to_string())?;
            writer.close().map_err(|e| e.to_string())?;
            partitions.push(format!("log/{}/{}.parquet", rel, month));
        }
        relations.push(RelationEntry {
            name: rel.clone(),
            signature: signature_text(sig),
            resolution: sig.res.map(|r| r.to_string()).unwrap_or_default(),
            availability: if records { "recorded".into() } else { "bar_close".into() },
            rows: tuples.len(),
            partitions,
        });
    }
    let manifest = Manifest {
        name: name.to_string(),
        version: version.to_string(),
        as_of: ds.bundle_date().map(format_timestamp),
        processing_delay_seconds: 0,
        source: None,
        created_at: today(),
        relations,
        tests: None,
    };
    write_manifest(dir, &manifest)?;
    Ok(manifest)
}

/// Record the data's source and processing delay in a written bundle's
/// manifest (what the vendor adapters know and the builder does not).
pub fn annotate_manifest(dir: &Path, source: Option<String>, processing_delay_seconds: i64) -> Result<Manifest, String> {
    let mut m = read_manifest(dir)?;
    m.source = source;
    m.processing_delay_seconds = processing_delay_seconds;
    write_manifest(dir, &m)?;
    Ok(m)
}

fn write_manifest(dir: &Path, m: &Manifest) -> Result<(), String> {
    let text = serde_json::to_string_pretty(m).map_err(|e| e.to_string())?;
    fs::write(dir.join("manifest.json"), text).map_err(|e| format!("{}: {}", dir.join("manifest.json").display(), e))
}

pub fn read_manifest(dir: &Path) -> Result<Manifest, String> {
    let path = dir.join("manifest.json");
    let text = fs::read_to_string(&path).map_err(|e| format!("{}: {}", path.display(), e))?;
    serde_json::from_str(&text).map_err(|e| format!("{}: {}", path.display(), e))
}

/// Read a bundle's facts into a dataset, regardless of its tests.
fn read_facts(prog: &Program, dir: &Path, m: &Manifest) -> Result<Dataset, String> {
    let mut ds = Dataset::new();
    let table = dir.join("securities.csv");
    if table.exists() {
        let text = fs::read_to_string(&table).map_err(|e| e.to_string())?;
        for (ln, line) in text.lines().enumerate().skip(1).filter(|(_, l)| !l.trim().is_empty()) {
            let f: Vec<&str> = line.split(',').map(|x| x.trim()).collect();
            if f.len() < 4 {
                return Err(format!("{}:{}: short row", table.display(), ln + 1));
            }
            let from = parse_timestamp(f[2]).ok_or_else(|| format!("{}:{}: `{}` is not a timestamp", table.display(), ln + 1, f[2]))?;
            let to = if f[3].is_empty() {
                None
            } else {
                Some(parse_timestamp(f[3]).ok_or_else(|| format!("{}:{}: `{}` is not a timestamp", table.display(), ln + 1, f[3]))?)
            };
            ds.add_security(f[0], f[1], from, to);
        }
        let problems = check_identities(&ds);
        if !problems.is_empty() {
            return Err(format!("{}: {}", table.display(), problems.join("; ")));
        }
    }
    for entry in &m.relations {
        let Some(sig) = prog.relations.get(&entry.name) else { continue };
        let key_pos = sig.key_pos().unwrap_or(0);
        for part in &entry.partitions {
            let path = dir.join(part);
            let file = File::open(&path).map_err(|e| format!("{}: {}", path.display(), e))?;
            let reader = ParquetRecordBatchReaderBuilder::try_new(file)
                .map_err(|e| format!("{}: {}", path.display(), e))?
                .build()
                .map_err(|e| e.to_string())?;
            for batch in reader {
                let batch = batch.map_err(|e| format!("{}: {}", path.display(), e))?;
                let records = entry.availability == "recorded";
                let expected = sig.args.len() + usize::from(records);
                if batch.num_columns() != expected {
                    return Err(format!("{}: {} columns for the {}-argument relation `{}`", path.display(), batch.num_columns(), expected, entry.name));
                }
                for row in 0..batch.num_rows() {
                    let mut tuple = Vec::with_capacity(sig.args.len());
                    for (i, arg) in sig.args.iter().enumerate() {
                        let col = batch.column(i);
                        let v = match &arg.ty {
                            Ty::Equity => Value::Equity(ds.intern(string_at(col, row, &path)?)),
                            Ty::Label => Value::Label(ds.intern_label(string_at(col, row, &path)?)),
                            Ty::Timestamp => {
                                let t = int_at(col, row, &path)?;
                                Value::Time(if i == key_pos { sig.res.map(|r| bucket(r, t)).unwrap_or(t) } else { t })
                            }
                            Ty::Count => Value::Count(int_at(col, row, &path)?),
                            Ty::Quantity(_) => Value::Num(
                                col.as_any()
                                    .downcast_ref::<Float64Array>()
                                    .ok_or_else(|| format!("{}: column {} is not Float64", path.display(), i))?
                                    .value(row),
                            ),
                            t => return Err(format!("{}: a {} column cannot be read", path.display(), t)),
                        };
                        tuple.push(v);
                    }
                    if records {
                        let avail = int_at(batch.column(sig.args.len()), row, &path)?;
                        ds.add_available(&entry.name, tuple, avail);
                    } else {
                        ds.add(&entry.name, tuple);
                    }
                }
            }
        }
    }
    if prog.relations.get("ticker").map(|s| matches!(s.kind, Kind::Primitive { .. })).unwrap_or(false) {
        ds.derive_tickers();
    }
    ds.as_of = m.as_of.as_deref().and_then(parse_timestamp);
    Ok(ds)
}

fn string_at<'a>(col: &'a ArrayRef, row: usize, path: &Path) -> Result<&'a str, String> {
    Ok(col
        .as_any()
        .downcast_ref::<StringArray>()
        .ok_or_else(|| format!("{}: a column is not Utf8", path.display()))?
        .value(row))
}

fn int_at(col: &ArrayRef, row: usize, path: &Path) -> Result<i64, String> {
    Ok(col
        .as_any()
        .downcast_ref::<Int64Array>()
        .ok_or_else(|| format!("{}: a column is not Int64", path.display()))?
        .value(row))
}

/// Load a bundle for `prog`: the manifest must name the program's
/// environment and, when the program pins one, its version; the tests must
/// have passed unless `allow_untested`.
pub fn load_bundle(prog: &Program, dir: &Path, allow_untested: bool) -> Result<(Dataset, Manifest), String> {
    let m = read_manifest(dir)?;
    if m.name != prog.environment {
        return Err(format!(
            "bundle {} is `{}@{}`, but the strategy is written against environment `{}`",
            dir.display(),
            m.name,
            m.version,
            prog.environment
        ));
    }
    if let Some(v) = &prog.environment_version {
        if *v != m.version {
            return Err(format!("bundle {} is `{}@{}`, but the strategy names `{}@{}`", dir.display(), m.name, m.version, prog.environment, v));
        }
    }
    if m.tests.is_none() && !allow_untested {
        return Err(format!(
            "bundle {} (`{}@{}`) has not passed its bundle tests; run `abt bundle test` on it, or pass --untested to run on unvouched data",
            dir.display(),
            m.name,
            m.version
        ));
    }
    let ds = read_facts(prog, dir, &m)?;
    Ok((ds, m))
}

/// Run the bundle tests on a bundle in `dir` and record a pass in its
/// manifest (a failing bundle stays unmarked).
pub fn test_bundle(prog: &Program, dir: &Path) -> Result<(Manifest, Vec<TestResult>), String> {
    let mut m = read_manifest(dir)?;
    if m.name != prog.environment {
        return Err(format!(
            "bundle {} is `{}@{}`, but the program is written against `{}`",
            dir.display(),
            m.name,
            m.version,
            prog.environment
        ));
    }
    let ds = read_facts(prog, dir, &m)?;
    let results = run_tests_with(prog, &ds, &read_exceptions(dir)?);
    m.tests = if results.iter().all(|r| r.passed) {
        Some(TestReport {
            passed_at: today(),
            results: results.clone(),
        })
    } else {
        None
    };
    write_manifest(dir, &m)?;
    Ok((m, results))
}

/// The bundle tests of the data-bundle doc, section 3, on a dataset.
pub fn run_tests(prog: &Program, ds: &Dataset) -> Vec<TestResult> {
    run_tests_with(prog, ds, &[])
}

/// The bundle tests with a set of reviewed exceptions.
pub fn run_tests_with(prog: &Program, ds: &Dataset, exceptions: &[Exception]) -> Vec<TestResult> {
    let mut out = Vec::new();
    // Exceptions accepted by the test being pushed (set just before its push).
    let accepted_here = std::cell::Cell::new(0usize);
    let mut push = |name: &str, problems: Vec<String>| {
        let accepted = accepted_here.replace(0);
        out.push(TestResult {
            name: name.to_string(),
            passed: problems.is_empty(),
            detail: if problems.is_empty() && accepted > 0 {
                format!("ok ({} reviewed exception{} accepted)", accepted, if accepted == 1 { "" } else { "s" })
            } else if problems.is_empty() {
                "ok".into()
            } else {
                let more = if problems.len() > 5 { format!(" (+{} more)", problems.len() - 5) } else { String::new() };
                problems.iter().take(5).cloned().collect::<Vec<_>>().join("; ") + &more
            },
        })
    };
    let sym_name = |v: &Value| v.as_equity().map(|s| ds.symbols.name(s).to_string()).unwrap_or_default();
    // Identity.
    push("identity", check_identities(ds));
    // Bar labels: every temporal key is the label of its bucket at the relation's resolution.
    let mut problems = Vec::new();
    for (rel, sig) in &prog.relations {
        let (Some(k), Some(res), Some(tuples)) = (sig.key_pos(), sig.res, ds.facts.get(rel)) else {
            continue;
        };
        for tu in tuples {
            if let Some(t) = tu[k].as_time() {
                if bucket(res, t) != t {
                    problems.push(format!("{}: {} is not a {} bar label", rel, format_timestamp(t), res));
                }
            }
        }
    }
    push("bar labels", problems);
    // Positive prices.
    let mut problems = Vec::new();
    for (rel, sig) in &prog.relations {
        let Some(tuples) = ds.facts.get(rel) else { continue };
        for (i, arg) in sig.args.iter().enumerate() {
            if let Ty::Quantity(d) = &arg.ty {
                if d.c2 == 2 && d.s2 == -2 {
                    for tu in tuples {
                        if tu[i].as_f64().map(|x| x <= 0.0).unwrap_or(false) {
                            problems.push(format!(
                                "{}: {} at {} is not a positive price",
                                rel,
                                sym_name(&tu[0]),
                                tu.iter().find_map(|v| v.as_time()).map(format_timestamp).unwrap_or_default()
                            ));
                        }
                    }
                }
            }
        }
    }
    push("positive prices", problems);
    // Action reconciliation: the total-return ratio between consecutive
    // closes of a name, (close + that day's dividends) × split factor /
    // previous close, is within 25% of one on a split's ex-date and within
    // [0.6, 1.67] on any other bar; every split falls on a bar with a close.
    // A dividend going ex is an action that explains a gap (a vendor that
    // books a spin-off as a distribution of its value), quoted per share
    // after a same-day split. A reviewed exception excuses one bar.
    let excused: std::collections::BTreeSet<(&str, i64)> = exceptions.iter().filter(|e| e.test == "action reconciliation").map(|e| (e.security.as_str(), e.t)).collect();
    let mut accepted = 0usize;
    let mut problems = Vec::new();
    if let Some(closes) = ds.facts.get("close") {
        let mut series: BTreeMap<Value, BTreeMap<i64, f64>> = BTreeMap::new();
        for tu in closes {
            if let (Some(t), Some(p)) = (tu[1].as_time(), tu[2].as_f64()) {
                series.entry(tu[0].clone()).or_default().insert(t, p);
            }
        }
        let splits: BTreeMap<(Value, i64), f64> = ds
            .facts
            .get("split")
            .map(|tus| tus.iter().filter_map(|tu| Some(((tu[0].clone(), tu[1].as_time()?), tu[2].as_f64()?))).collect())
            .unwrap_or_default();
        let mut dividends: BTreeMap<(Value, i64), f64> = BTreeMap::new();
        for tu in ds.facts.get("dividend").map(|v| v.as_slice()).unwrap_or(&[]) {
            if let (Some(ex), Some(amount)) = (tu.get(2).and_then(|v| v.as_time()), tu.get(4).and_then(|v| v.as_f64())) {
                *dividends.entry((tu[0].clone(), ex)).or_insert(0.0) += amount;
            }
        }
        for (sym, s) in &series {
            let name = sym_name(sym);
            let mut prev: Option<f64> = None;
            for (&t, &p) in s {
                if let Some(p0) = prev {
                    let div = dividends.get(&(sym.clone(), t)).copied().unwrap_or(0.0);
                    let split = splits.get(&(sym.clone(), t)).copied();
                    let ratio = (p + div) * split.unwrap_or(1.0) / p0;
                    let problem = match split {
                        Some(f) if (ratio - 1.0).abs() > 0.25 => Some(format!("{} at {}: a {}:1 split but the close moved from {} to {}", name, format_timestamp(t), f, p0, p)),
                        None if !(0.6..=1.67).contains(&ratio) => Some(if div > 0.0 {
                            format!(
                                "{} at {}: the close moved from {} to {} and a dividend of {} does not explain it",
                                name,
                                format_timestamp(t),
                                p0,
                                p,
                                div
                            )
                        } else {
                            format!("{} at {}: the close moved from {} to {} with no action to explain it", name, format_timestamp(t), p0, p)
                        }),
                        _ => None,
                    };
                    if let Some(problem) = problem {
                        if excused.contains(&(name.as_str(), t)) {
                            accepted += 1;
                        } else {
                            problems.push(problem);
                        }
                    }
                }
                prev = Some(p);
            }
        }
        for ((sym, t), f) in &splits {
            if !series.get(sym).map(|s| s.contains_key(t)).unwrap_or(false) {
                problems.push(format!("{} at {}: a {}:1 split on a bar with no close", sym_name(sym), format_timestamp(*t), f));
            }
        }
    }
    accepted_here.set(accepted);
    push("action reconciliation", problems);
    // Delisting coverage: a name whose last universe bar is before the data's
    // last bar has a delisted tuple.
    let mut problems = Vec::new();
    if let Some(universe) = ds.facts.get("universe") {
        let last_bar = universe.iter().filter_map(|tu| tu[1].as_time()).max();
        let mut last_seen: BTreeMap<Value, i64> = BTreeMap::new();
        for tu in universe {
            if let Some(t) = tu[1].as_time() {
                let e = last_seen.entry(tu[0].clone()).or_insert(t);
                *e = (*e).max(t);
            }
        }
        let delisted: std::collections::BTreeSet<Value> = ds.facts.get("delisted").map(|tus| tus.iter().map(|tu| tu[0].clone()).collect()).unwrap_or_default();
        if let Some(last_bar) = last_bar {
            for (sym, t) in &last_seen {
                if *t < last_bar && !delisted.contains(sym) {
                    problems.push(format!("{} leaves the universe at {} with no delisted record", sym_name(sym), format_timestamp(*t)));
                }
            }
        }
    }
    push("delisting coverage", problems);
    // Membership: a member tuple is for a name in the universe that bar.
    let mut problems = Vec::new();
    if let (Some(members), Some(universe)) = (ds.facts.get("member"), ds.facts.get("universe")) {
        let listed: std::collections::BTreeSet<(Value, i64)> = universe.iter().filter_map(|tu| Some((tu[0].clone(), tu[1].as_time()?))).collect();
        for tu in members {
            if let Some(t) = tu[1].as_time() {
                if !listed.contains(&(tu[0].clone(), t)) {
                    problems.push(format!("{} is a member at {} but not in the universe", sym_name(&tu[0]), format_timestamp(t)));
                }
            }
        }
    }
    push("membership", problems);
    out
}

/// The bundle directory's expected layout, for messages.
pub fn layout() -> Vec<PathBuf> {
    vec![
        PathBuf::from("manifest.json"),
        PathBuf::from("securities.csv"),
        PathBuf::from("log/<relation>/<YYYY-MM>.parquet"),
        PathBuf::from("snapshots/"),
    ]
}
