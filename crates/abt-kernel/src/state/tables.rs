//! The state tables (compiler design, section 7.2): `extension(R)`,
//! `summary`, `carried(R)`, `derivation`, `failure`, `coverage`, `plan`,
//! `book` and `binding`, each with a stable schema, as text, JSON, Arrow
//! IPC or Parquet. Every row that concerns a rule carries the rule's label
//! in the author's names.

use std::collections::BTreeMap;
use std::path::Path;

use crate::check::Program;
use crate::ir::{Kind, Mode, Ty};
use crate::kernel::time::format_timestamp;
use crate::kernel::{Dataset, Kernel, RunError, RunResult, Value};
use crate::plan::Plan;
use crate::table::{write_ipc, write_table, Col};

/// One cell of a state table.
#[derive(Clone, Debug, PartialEq)]
pub enum Cell {
    Null,
    Str(String),
    Int(i64),
    Float(f64),
    Bool(bool),
    /// Seconds since the epoch, rendered as a timestamp.
    Time(i64),
}

impl Cell {
    pub fn text(&self) -> String {
        match self {
            Cell::Null => String::new(),
            Cell::Str(s) => s.clone(),
            Cell::Int(i) => i.to_string(),
            Cell::Float(x) => format!("{}", x),
            Cell::Bool(b) => b.to_string(),
            Cell::Time(t) => format_timestamp(*t),
        }
    }
    pub fn json(&self) -> serde_json::Value {
        match self {
            Cell::Null => serde_json::Value::Null,
            Cell::Str(s) => serde_json::Value::String(s.clone()),
            Cell::Int(i) => serde_json::json!(i),
            Cell::Float(x) => serde_json::json!(x),
            Cell::Bool(b) => serde_json::json!(b),
            Cell::Time(t) => serde_json::Value::String(format_timestamp(*t)),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColType {
    Str,
    Int,
    Float,
    Bool,
    Time,
}

impl ColType {
    pub fn text(self) -> &'static str {
        match self {
            ColType::Str => "text",
            ColType::Int => "integer",
            ColType::Float => "number",
            ColType::Bool => "boolean",
            ColType::Time => "timestamp",
        }
    }
}

/// A table: a name, typed columns, rows, and notes about how it was built.
#[derive(Clone, Debug)]
pub struct Table {
    pub name: String,
    pub columns: Vec<(String, ColType)>,
    pub rows: Vec<Vec<Cell>>,
    pub notes: Vec<String>,
}

impl Table {
    pub fn new(name: &str, columns: &[(&str, ColType)]) -> Table {
        Table {
            name: name.into(),
            columns: columns.iter().map(|(n, t)| (n.to_string(), *t)).collect(),
            rows: Vec::new(),
            notes: Vec::new(),
        }
    }
    pub fn push(&mut self, row: Vec<Cell>) {
        debug_assert_eq!(row.len(), self.columns.len(), "{}: a row of {} cells in {} columns", self.name, row.len(), self.columns.len());
        self.rows.push(row);
    }
    pub fn len(&self) -> usize {
        self.rows.len()
    }
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }
    /// The schema line: `name(col: type, ...)`.
    pub fn schema(&self) -> String {
        format!("{}({})", self.name, self.columns.iter().map(|(n, t)| format!("{}: {}", n, t.text())).collect::<Vec<_>>().join(", "))
    }
    /// Aligned text: the schema, the rows, the notes.
    pub fn text(&self) -> String {
        let mut out = format!("{}: {} row(s)\n", self.schema(), self.rows.len());
        let cells: Vec<Vec<String>> = self.rows.iter().map(|r| r.iter().map(|c| c.text()).collect()).collect();
        let mut widths: Vec<usize> = self.columns.iter().map(|(n, _)| n.chars().count()).collect();
        for r in &cells {
            for (i, c) in r.iter().enumerate() {
                widths[i] = widths[i].max(c.chars().count().min(60));
            }
        }
        let line = |cells: &[String]| -> String {
            cells
                .iter()
                .enumerate()
                .map(|(i, c)| {
                    let c: String = if c.chars().count() > 60 { c.chars().take(57).collect::<String>() + "…" } else { c.clone() };
                    format!("{:<w$}", c, w = widths[i])
                })
                .collect::<Vec<_>>()
                .join("  ")
                .trim_end()
                .to_string()
        };
        let header: Vec<String> = self.columns.iter().map(|(n, _)| n.clone()).collect();
        out.push_str("  ");
        out.push_str(&line(&header));
        out.push('\n');
        for r in &cells {
            out.push_str("  ");
            out.push_str(&line(r));
            out.push('\n');
        }
        for n in &self.notes {
            out.push_str(&format!("  note: {}\n", n));
        }
        out
    }
    pub fn json(&self) -> serde_json::Value {
        serde_json::json!({
            "table": self.name,
            "columns": self.columns.iter().map(|(n, t)| serde_json::json!({"name": n, "type": t.text()})).collect::<Vec<_>>(),
            "rows": self.rows.iter().map(|r| serde_json::Value::Array(r.iter().map(|c| c.json()).collect())).collect::<Vec<_>>(),
            "notes": self.notes,
        })
    }
    /// The columns as the writers take them (a cell of another type than
    /// its column is null).
    pub fn cols(&self) -> Vec<(&str, Col)> {
        self.columns
            .iter()
            .enumerate()
            .map(|(i, (name, ty))| {
                let col = match ty {
                    ColType::Str => Col::Str(
                        self.rows
                            .iter()
                            .map(|r| match &r[i] {
                                Cell::Null => None,
                                c => Some(c.text()),
                            })
                            .collect(),
                    ),
                    ColType::Int => Col::Int(
                        self.rows
                            .iter()
                            .map(|r| match &r[i] {
                                Cell::Int(v) => Some(*v),
                                Cell::Float(x) if x.fract() == 0.0 => Some(*x as i64),
                                _ => None,
                            })
                            .collect(),
                    ),
                    ColType::Float => Col::Float(
                        self.rows
                            .iter()
                            .map(|r| match &r[i] {
                                Cell::Float(x) => Some(*x),
                                Cell::Int(v) => Some(*v as f64),
                                _ => None,
                            })
                            .collect(),
                    ),
                    ColType::Bool => Col::Bool(
                        self.rows
                            .iter()
                            .map(|r| match &r[i] {
                                Cell::Bool(b) => Some(*b),
                                _ => None,
                            })
                            .collect(),
                    ),
                    ColType::Time => Col::Time(
                        self.rows
                            .iter()
                            .map(|r| match &r[i] {
                                Cell::Time(t) => Some(*t),
                                _ => None,
                            })
                            .collect(),
                    ),
                };
                (name.as_str(), col)
            })
            .collect()
    }
    /// Write the table: Parquet for a `.parquet` path, Arrow IPC otherwise.
    pub fn write(&self, path: &Path) -> Result<(), String> {
        if path.extension().map(|e| e == "parquet").unwrap_or(false) {
            write_table(path, self.cols())
        } else {
            write_ipc(path, self.cols())
        }
    }
}

/// The column type of a signature argument.
pub fn col_type(ty: &Ty) -> ColType {
    match ty {
        Ty::Timestamp => ColType::Time,
        Ty::Count | Ty::IntLit => ColType::Int,
        Ty::Quantity(_) => ColType::Float,
        _ => ColType::Str,
    }
}

/// A value as a cell of a column type.
pub fn cell_of(k: &Kernel, v: &Value, ty: ColType) -> Cell {
    match (v, ty) {
        (Value::Time(t), ColType::Time) => Cell::Time(*t),
        (Value::Num(x), ColType::Float) => Cell::Float(*x),
        (Value::Count(c), ColType::Float) => Cell::Float(*c as f64),
        (Value::Count(c), ColType::Int) => Cell::Int(*c),
        (Value::Num(x), ColType::Int) if x.fract() == 0.0 => Cell::Int(*x as i64),
        (v, _) => Cell::Str(k.show(v)),
    }
}

/// The coverage table: what every relation derived over the run.
pub fn coverage(result: &RunResult) -> Table {
    let mut t = Table::new(
        "coverage",
        &[
            ("relation", ColType::Str),
            ("derived", ColType::Bool),
            ("calls", ColType::Int),
            ("nonempty", ColType::Int),
            ("tuples", ColType::Int),
        ],
    );
    for c in &result.coverage {
        t.push(vec![
            Cell::Str(c.name.clone()),
            Cell::Bool(c.derived),
            Cell::Int(c.calls as i64),
            Cell::Int(c.nonempty as i64),
            Cell::Int(c.tuples as i64),
        ]);
    }
    t
}

/// The plan table: every literal of every rule in its evaluation order
/// with its estimates and, when a run supplied them, the observed counts.
pub fn plan(plan: &Plan) -> Table {
    let mut t = Table::new(
        "plan",
        &[
            ("rule", ColType::Str),
            ("position", ColType::Int),
            ("literal", ColType::Int),
            ("kind", ColType::Str),
            ("text", ColType::Str),
            ("card_in", ColType::Float),
            ("card_out", ColType::Float),
            ("cost", ColType::Float),
            ("selectivity", ColType::Float),
            ("observed_in", ColType::Int),
            ("observed_out", ColType::Int),
            ("observed_emptied", ColType::Int),
            ("infeasible", ColType::Bool),
            ("elided", ColType::Bool),
        ],
    );
    for rp in &plan.rules {
        for (pos, l) in rp.literals.iter().enumerate() {
            let (oi, oo, oe) = match l.observed {
                Some((a, b, c)) => (Cell::Int(a as i64), Cell::Int(b as i64), Cell::Int(c as i64)),
                None => (Cell::Null, Cell::Null, Cell::Null),
            };
            t.push(vec![
                Cell::Str(rp.label.clone()),
                Cell::Int(pos as i64),
                Cell::Int(l.index as i64),
                Cell::Str(l.kind.clone()),
                Cell::Str(l.text.clone()),
                Cell::Float(l.card_in),
                Cell::Float(l.card_out),
                Cell::Float(l.cost),
                Cell::Float(l.selectivity),
                oi,
                oo,
                oe,
                Cell::Bool(rp.infeasible),
                Cell::Bool(rp.elided),
            ]);
        }
    }
    t.notes
        .push(format!("plan `{}` under numerics {}; statistics from {}", plan.label, plan.numerics.name(), plan.statistics));
    t
}

/// The book: the executor's ledger at every bar.
pub fn book(result: &RunResult) -> Table {
    let mut t = Table::new(
        "book",
        &[
            ("bar", ColType::Time),
            ("cash", ColType::Float),
            ("gross", ColType::Float),
            ("net", ColType::Float),
            ("equity", ColType::Float),
            ("leverage", ColType::Float),
        ],
    );
    for e in &result.exposure {
        t.push(vec![
            Cell::Time(e.t),
            Cell::Float(e.cash),
            Cell::Float(e.gross),
            Cell::Float(e.net),
            Cell::Float(e.equity),
            Cell::Float(e.leverage),
        ]);
    }
    t
}

/// The binding: every relation the data supplies, with its row count and
/// the span of its temporal key.
pub fn binding(prog: &Program, ds: &Dataset) -> Table {
    let mut t = Table::new(
        "binding",
        &[
            ("relation", ColType::Str),
            ("kind", ColType::Str),
            ("rows", ColType::Int),
            ("first_bar", ColType::Time),
            ("last_bar", ColType::Time),
            ("availability", ColType::Str),
        ],
    );
    for (name, tuples) in &ds.facts {
        let sig = prog.relations.get(name);
        let kind = match sig.map(|s| &s.kind) {
            Some(Kind::Primitive { class: env }) => format!("primitive ({})", env),
            Some(Kind::Executor) => "executor".into(),
            Some(Kind::KernelState) => "kernel".into(),
            Some(Kind::Derived { .. }) => "derived".into(),
            Some(Kind::Output) => "output".into(),
            None => "not in the program".into(),
        };
        let key = sig.and_then(|s| s.key_pos());
        let times: Vec<i64> = key.map(|k| tuples.iter().filter_map(|tu| tu.get(k).and_then(|v| v.as_time())).collect()).unwrap_or_default();
        t.push(vec![
            Cell::Str(name.clone()),
            Cell::Str(kind),
            Cell::Int(tuples.len() as i64),
            times.iter().min().map(|t| Cell::Time(*t)).unwrap_or(Cell::Null),
            times.iter().max().map(|t| Cell::Time(*t)).unwrap_or(Cell::Null),
            Cell::Str(if ds.available_at.contains_key(name) {
                "recorded per tuple".into()
            } else {
                "at the bar's close".into()
            }),
        ]);
    }
    t.notes.push(format!("{} symbol(s), {} label(s)", ds.symbols.len(), ds.labels.len()));
    t
}

/// The input values a query of `rel` needs: the given ones, or the symbol
/// when the entity is the only input, or none.
fn input_sets(k: &Kernel, prog: &Program, rel: &str, inputs: Option<&[Value]>) -> Result<Vec<Vec<Value>>, RunError> {
    let Some(sig) = prog.relations.get(rel) else {
        return Err(RunError::Request(format!("`{}` is not a relation of {}", rel, prog.strategy)));
    };
    let need: Vec<&crate::ir::Arg> = sig.args.iter().filter(|a| false && a.mode == Mode::Key).collect();
    match inputs {
        Some(v) if v.len() == need.len() => Ok(vec![v.to_vec()]),
        Some(v) => Err(RunError::Request(format!("`{}` takes {} input(s), {} given", rel, need.len(), v.len()))),
        None if need.is_empty() => Ok(vec![vec![]]),
        None if matches!(need.as_slice(), [a] if a.ty.is_entity()) => Ok((0..k.symbol_names().len()).map(|i| vec![Value::Equity(i as u32)]).collect()),
        None => Err(RunError::Request(format!(
            "`{}` takes the inputs {}; name them with --inputs",
            rel,
            need.iter().map(|a| format!("{}: {}", a.name, a.ty)).collect::<Vec<_>>().join(", ")
        ))),
    }
}

/// The tuples of `rel` at bar `t` over every input set.
pub fn tuples_at(k: &mut Kernel, prog: &Program, rel: &str, t: i64, inputs: Option<&[Value]>) -> Result<(Vec<Vec<Value>>, Vec<String>), RunError> {
    let mut out = Vec::new();
    let mut notes = Vec::new();
    for set in input_sets(k, prog, rel, inputs)? {
        match k.query(rel, t, &set) {
            Ok(tus) => out.extend(tus),
            Err(RunError::Arithmetic { rule, bindings, expr, message }) => notes.push(format!("{}: partial arithmetic in {} at [{}]: `{}`: {}", format_timestamp(t), rule, bindings, expr, message)),
            Err(e) => return Err(e),
        }
    }
    Ok((out, notes))
}

/// The bars of a relation's own time domain inside a run's window.
pub fn own_bars(k: &Kernel, prog: &Program, rel: &str, bars: &[i64]) -> Vec<i64> {
    let (Some(&first), Some(&last)) = (bars.first(), bars.last()) else { return Vec::new() };
    let cal = prog.cal_of(rel);
    k.bars_at(&cal).into_iter().filter(|&b| b >= first && b <= last).collect()
}

fn signature_columns(prog: &Program, rel: &str) -> Vec<(String, ColType)> {
    prog.relations.get(rel).map(|s| s.args.iter().map(|a| (a.name.clone(), col_type(&a.ty))).collect()).unwrap_or_default()
}

/// `extension(R)`: every tuple at every bar of `bars` (the relation's own
/// resolution), sorted by bar then by the tuple's text.
pub fn extension(k: &mut Kernel, prog: &Program, rel: &str, bars: &[i64], inputs: Option<&[Value]>) -> Result<Table, RunError> {
    let cols = signature_columns(prog, rel);
    let spec: Vec<(&str, ColType)> = cols.iter().map(|(n, t)| (n.as_str(), *t)).collect();
    let mut t = Table::new(&format!("extension({})", rel), &spec);
    let mut rows: Vec<(i64, String, Vec<Cell>)> = Vec::new();
    for bar in own_bars(k, prog, rel, bars) {
        let (tuples, notes) = tuples_at(k, prog, rel, bar, inputs)?;
        t.notes.extend(notes);
        for tu in tuples {
            let cells: Vec<Cell> = tu.iter().zip(cols.iter()).map(|(v, (_, ty))| cell_of(k, v, *ty)).collect();
            let text = cells.iter().map(|c| c.text()).collect::<Vec<_>>().join(",");
            rows.push((bar, text, cells));
        }
    }
    rows.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    for (_, _, cells) in rows {
        t.push(cells);
    }
    Ok(t)
}

/// `carried(R)`: at every bar, for every identity the relation held a
/// tuple for at an earlier bar, the latest such tuple (what a rule reading
/// the relation through `prev` sees), with the bar it came from.
pub fn carried(k: &mut Kernel, prog: &Program, rel: &str, bars: &[i64], inputs: Option<&[Value]>) -> Result<Table, RunError> {
    let Some(sig) = prog.relations.get(rel) else {
        return Err(RunError::Request(format!("`{}` is not a relation of {}", rel, prog.strategy)));
    };
    let key = sig.key_pos().unwrap_or(usize::MAX);
    let ids: Vec<usize> = sig.identity_positions().into_iter().filter(|&i| i != key).collect();
    let values: Vec<usize> = (0..sig.args.len()).filter(|i| *i != key && !ids.contains(i)).collect();
    let mut spec: Vec<(String, ColType)> = ids.iter().map(|&i| (sig.args[i].name.clone(), col_type(&sig.args[i].ty))).collect();
    spec.push(("bar".into(), ColType::Time));
    spec.push(("carried_from".into(), ColType::Time));
    spec.extend(values.iter().map(|&i| (sig.args[i].name.clone(), col_type(&sig.args[i].ty))));
    let spec_ref: Vec<(&str, ColType)> = spec.iter().map(|(n, t)| (n.as_str(), *t)).collect();
    let mut t = Table::new(&format!("carried({})", rel), &spec_ref);
    let mut last: BTreeMap<String, (i64, Vec<Value>)> = BTreeMap::new();
    for bar in own_bars(k, prog, rel, bars) {
        for (identity, (from, tu)) in &last {
            let _ = identity;
            let mut row: Vec<Cell> = ids.iter().map(|&i| cell_of(k, &tu[i], col_type(&sig.args[i].ty))).collect();
            row.push(Cell::Time(bar));
            row.push(Cell::Time(*from));
            row.extend(values.iter().map(|&i| cell_of(k, &tu[i], col_type(&sig.args[i].ty))));
            t.push(row);
        }
        let (tuples, notes) = tuples_at(k, prog, rel, bar, inputs)?;
        t.notes.extend(notes);
        for tu in tuples {
            let identity = ids.iter().map(|&i| k.show(&tu[i])).collect::<Vec<_>>().join(",");
            last.insert(identity, (bar, tu));
        }
    }
    if !prog.infos.iter().zip(prog.rules.iter()).any(|(_, r)| r.head.name == rel) {
        t.notes.push(format!("`{}` has no rules: the carried value is the supplied tuple's", rel));
    }
    Ok(t)
}

/// `summary`: the window state of every aggregation group at every bar it
/// served, as the kernel recorded it (`Kernel::record_summaries`).
pub fn summary(k: &Kernel, prog: &Program) -> Table {
    let mut t = Table::new(
        "summary",
        &[
            ("rule", ColType::Str),
            ("literal", ColType::Int),
            ("aggregate", ColType::Str),
            ("group", ColType::Str),
            ("bar", ColType::Time),
            ("rows", ColType::Int),
            ("path", ColType::Str),
            ("shift_x", ColType::Float),
            ("shift_y", ColType::Float),
            ("sum_x", ColType::Float),
            ("sum_y", ColType::Float),
            ("sum_xx", ColType::Float),
            ("sum_yy", ColType::Float),
            ("sum_xy", ColType::Float),
            ("value", ColType::Float),
        ],
    );
    for r in k.summary_rows() {
        let group = r.outer.iter().map(|v| v.as_ref().map(|v| k.show(v)).unwrap_or_else(|| "_".into())).collect::<Vec<_>>().join(", ");
        let f = |x: Option<f64>| x.map(Cell::Float).unwrap_or(Cell::Null);
        t.push(vec![
            Cell::Str(prog.rule_label(r.rule)),
            Cell::Int(r.literal as i64),
            Cell::Str(r.agg.clone()),
            Cell::Str(group),
            Cell::Time(r.bar),
            Cell::Int(r.rows as i64),
            Cell::Str(r.path.into()),
            f(r.shift.map(|s| s.0)),
            f(r.shift.map(|s| s.1)),
            f(r.sums.map(|s| s[0])),
            f(r.sums.map(|s| s[1])),
            f(r.sums.map(|s| s[2])),
            f(r.sums.map(|s| s[3])),
            f(r.sums.map(|s| s[4])),
            f(r.value),
        ]);
    }
    if k.summary_rows().is_empty() {
        t.notes
            .push("no summary recorded: the run was not recording (`Kernel::record_summaries`), or no windowed aggregation ran through a cache".into());
    }
    t
}

/// `derivation` at a bar: one witness per derived tuple of `rel`.
pub fn derivation(k: &mut Kernel, prog: &Program, rel: &str, bar: i64, inputs: Option<&[Value]>) -> Result<Table, RunError> {
    let mut t = Table::new("derivation", &[("tuple", ColType::Str), ("bar", ColType::Time), ("rule", ColType::Str), ("body", ColType::Str)]);
    let (tuples, notes) = tuples_at(k, prog, rel, bar, inputs)?;
    t.notes.extend(notes);
    for tu in tuples {
        let id = super::provenance::tuple_id(k, rel, &tu, bar);
        match super::provenance::witness(k, prog, rel, bar, &tu)? {
            Some(w) => t.push(vec![Cell::Str(id), Cell::Time(bar), Cell::Str(w.rule), Cell::Str(w.body.join("; "))]),
            None => t.push(vec![
                Cell::Str(id),
                Cell::Time(bar),
                Cell::Null,
                Cell::Str(if prog.rules_for(rel).is_empty() { "supplied".into() } else { "no rule re-derives it".into() }),
            ]),
        }
    }
    Ok(t)
}

/// `failure` at a bar: for every rule of `rel` and every attempted
/// identity, the first excluding literal and the bindings at that point.
pub fn failure(k: &mut Kernel, prog: &Program, rel: &str, bar: i64, inputs: Option<&[Value]>, symbol: Option<Value>) -> Result<Table, RunError> {
    let mut t = Table::new(
        "failure",
        &[
            ("rule", ColType::Str),
            ("identity", ColType::Str),
            ("bar", ColType::Time),
            ("literal", ColType::Int),
            ("literal_text", ColType::Str),
            ("bindings", ColType::Str),
            ("absence_driven", ColType::Bool),
        ],
    );
    for f in super::provenance::failures(k, prog, rel, bar, inputs, symbol)? {
        if f.fired {
            continue;
        }
        let (li, text) = match (&f.literal, &f.error) {
            (Some((i, text)), _) => (Cell::Int(*i as i64), Cell::Str(text.clone())),
            (None, Some(e)) => (Cell::Null, Cell::Str(format!("halted: {}", e))),
            (None, None) => (Cell::Null, Cell::Null),
        };
        t.push(vec![
            Cell::Str(f.rule.clone()),
            Cell::Str(f.identity.clone()),
            Cell::Time(bar),
            li,
            text,
            Cell::Str(f.bindings.iter().map(|(v, x)| format!("{} = {}", v, x)).collect::<Vec<_>>().join(", ")),
            Cell::Bool(f.absence_driven),
        ]);
    }
    Ok(t)
}

/// `materialise(R, bar)`: the relation's tuples at one bar, evaluated from
/// its rules whether or not the plan computed it (section 2.4).
pub fn materialise(k: &mut Kernel, prog: &Program, rel: &str, bar: i64, inputs: Option<&[Value]>, elided: bool) -> Result<Table, RunError> {
    let mut t = extension(k, prog, rel, &[bar, bar], inputs)?;
    t.name = format!("materialise({})", rel);
    t.notes.push(if elided {
        format!(
            "`{}` is elided in the plan (nothing demanded reaches it): evaluated here from its rules at {}",
            rel,
            format_timestamp(bar)
        )
    } else {
        format!("`{}` is computed by the plan: these are the run's tuples at {}", rel, format_timestamp(bar))
    });
    Ok(t)
}

/// The bars a run's tables are recorded at by default: the last bar and
/// up to nine more, evenly spaced (section 7.3, the sampled bars).
pub fn sampled_bars(bars: &[i64], count: usize) -> Vec<i64> {
    if bars.is_empty() || count == 0 {
        return Vec::new();
    }
    let count = count.min(bars.len());
    let mut out: Vec<i64> = (0..count).map(|i| bars[(i * (bars.len() - 1)) / (count.max(2) - 1).max(1)]).collect();
    out.push(*bars.last().unwrap());
    out.sort_unstable();
    out.dedup();
    out
}
