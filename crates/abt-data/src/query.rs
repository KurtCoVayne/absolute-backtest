//! The query forms (D-4, P-3.2): typed requests over a view, each a
//! deterministic function of (view, as-of sequence, receipt filter,
//! arguments) (Proposition D-4.2). Answers are Arrow tables in the
//! relation's schema, labels as dictionaries, `seq` appended on request,
//! the latest admitted revision of each fact unless all are asked for.

use std::collections::BTreeMap;
use std::sync::Arc;

use abt_wire::generated::abt::wire::{Atom, ColumnType, Constant, ConstantKind, ExplainResponse, Identity, Op, Predicate, QueryForm, RelationSchema, View};
use arrow::array::{Array, ArrayRef, AsArray, BooleanArray, Int64Array, RecordBatch};
use arrow::datatypes::{DataType, Field, Schema, SchemaRef};

use crate::error::DataError;
use crate::rows::{self, layout, stored_schema};
use crate::store::{RowFilter, Store};

/// The versions a view reads, by relation (D-4: a version list or a
/// binding, with an as-of sequence number).
#[derive(Clone, Debug, Default)]
pub struct ResolvedView {
    pub versions: BTreeMap<String, String>,
    pub as_of_seq: Option<u64>,
}

impl ResolvedView {
    /// A view of the store: explicit versions, else a stored binding's.
    /// A relation the view does not name is read at its current version
    /// (the transitional resolver of M-2: every relation at its current
    /// version, docs/spec/05-migration.md).
    pub fn of(store: &Store, view: Option<&View>) -> Result<ResolvedView, DataError> {
        let mut out = ResolvedView::default();
        if let Some(v) = view {
            out.as_of_seq = v.as_of_seq;
            if let Some(b) = &v.binding {
                let binding = store.registry.binding(b).ok_or_else(|| DataError::NotFound(format!("no binding {}", b)))?;
                for p in binding.pairs.unwrap_or_default() {
                    out.versions.insert(p.relation, p.version);
                }
            }
            for p in v.versions.clone().unwrap_or_default() {
                out.versions.insert(p.relation, p.version);
            }
        }
        Ok(out)
    }

    /// The version of a relation in the view.
    pub fn version(&self, store: &Store, relation: &str) -> Result<String, DataError> {
        if let Some(v) = self.versions.get(relation) {
            return Ok(v.clone());
        }
        store
            .registry
            .current(relation, None)
            .map(|v| v.to_string())
            .ok_or_else(|| DataError::UnknownRelation(relation.to_string()))
    }
}

/// An identity's key in the store (an entity id, or a label's code).
fn identity_key(store: &Store, id: &Identity) -> Result<u64, DataError> {
    let v = id.values.first().ok_or_else(|| DataError::Malformed("an empty identity".into()))?;
    Ok(match v.kind {
        ConstantKind::Entity | ConstantKind::Count => v.int as u64,
        ConstantKind::Label => store.registry.label_code(v.text.as_deref().unwrap_or("")).map(|c| c as u64).unwrap_or(u64::MAX),
        _ => return Err(DataError::Malformed("an identity is an entity or a label".into())),
    })
}

/// A constant as an i128 for comparison with a stored column: times and
/// counts as integers, labels as codes; floats are compared apart.
fn constant_int(store: &Store, c: &Constant) -> Option<i128> {
    match c.kind {
        ConstantKind::Entity | ConstantKind::Count | ConstantKind::Time => Some(c.int as i128),
        ConstantKind::Label => store.registry.label_code(c.text.as_deref().unwrap_or("")).map(|x| x as i128).or(Some(-1)),
        ConstantKind::Bool => Some(c.int as i128),
        ConstantKind::Num => None,
    }
}

/// Definition D-4.1: a conjunction of disjunctions of atoms over the
/// relation's columns; no joins, no arithmetic.
fn predicate_mask(store: &Store, s: &RelationSchema, b: &RecordBatch, p: &Predicate) -> Result<BooleanArray, DataError> {
    let n = b.num_rows();
    let mut keep = vec![true; n];
    for d in p.all.clone().unwrap_or_default() {
        let mut any = vec![false; n];
        for a in &d.atoms {
            let m = atom_mask(store, s, b, a)?;
            for i in 0..n {
                any[i] |= m[i];
            }
        }
        for i in 0..n {
            keep[i] &= any[i];
        }
    }
    Ok(BooleanArray::from(keep))
}

fn atom_mask(store: &Store, s: &RelationSchema, b: &RecordBatch, a: &Atom) -> Result<Vec<bool>, DataError> {
    let ci = s
        .columns
        .iter()
        .position(|c| c.name == a.column)
        .ok_or_else(|| DataError::Malformed(format!("`{}` has no column `{}`", s.name, a.column)))?;
    let col = b.column(ci);
    let ty = s.columns[ci].ty;
    let values = a.values.clone().unwrap_or_default();
    let n = b.num_rows();
    if a.op == Op::IsNull {
        return Ok((0..n).map(|i| col.is_null(i)).collect());
    }
    let cmp = |x: std::cmp::Ordering| -> bool {
        match a.op {
            Op::Eq | Op::In => x.is_eq(),
            Op::Ne => x.is_ne(),
            Op::Lt => x.is_lt(),
            Op::Le => x.is_le(),
            Op::Gt => x.is_gt(),
            Op::Ge => x.is_ge(),
            Op::IsNull => false,
        }
    };
    let out = (0..n)
        .map(|i| {
            if col.is_null(i) {
                return false;
            }
            values.iter().any(|v| {
                let ord = match ty {
                    ColumnType::Num => {
                        let x = col.as_primitive::<arrow::datatypes::Float64Type>().value(i);
                        let c = if v.kind == ConstantKind::Num { v.num } else { v.int as f64 };
                        x.partial_cmp(&c)
                    }
                    _ => {
                        let x: i128 = match ty {
                            ColumnType::Entity => col.as_primitive::<arrow::datatypes::UInt64Type>().value(i) as i128,
                            ColumnType::Time => col.as_primitive::<arrow::datatypes::TimestampNanosecondType>().value(i) as i128,
                            ColumnType::Count => col.as_primitive::<arrow::datatypes::Int64Type>().value(i) as i128,
                            ColumnType::Label => col.as_primitive::<arrow::datatypes::UInt32Type>().value(i) as i128,
                            ColumnType::Bool => col.as_boolean().value(i) as i128,
                            ColumnType::Bytes | ColumnType::Num => return false,
                        };
                        constant_int(store, v).map(|c| x.cmp(&c))
                    }
                };
                ord.is_some_and(cmp)
            })
        })
        .collect();
    Ok(out)
}

/// What a form reads: the relation, its rows' filter, and what it makes of
/// them.
fn plan(store: &Store, form: &QueryForm, r_max: Option<i64>, as_of: Option<u64>) -> Result<(String, RowFilter), DataError> {
    let mut f = RowFilter {
        r_max,
        as_of_seq: as_of,
        latest_only: true,
        ..RowFilter::default()
    };
    let relation = match form {
        QueryForm::Point(p) => {
            f.t0 = Some(p.t);
            f.t1 = Some(p.t);
            f.identity = p.identity.as_ref().map(|i| identity_key(store, i)).transpose()?.map(|k| vec![k]);
            p.relation.clone()
        }
        QueryForm::Latest(p) => {
            f.t1 = Some(p.t_max);
            f.identity = p.identity.as_ref().map(|i| identity_key(store, i)).transpose()?.map(|k| vec![k]);
            p.relation.clone()
        }
        QueryForm::Series(p) => {
            f.t0 = Some(p.t0);
            f.t1 = Some(p.t1);
            f.identity = p.identity.as_ref().map(|i| identity_key(store, i)).transpose()?.map(|k| vec![k]);
            p.relation.clone()
        }
        QueryForm::Section(p) => {
            f.t0 = Some(p.t);
            f.t1 = Some(p.t);
            p.relation.clone()
        }
        QueryForm::Scan(p) => {
            f.t0 = Some(p.t0);
            f.t1 = Some(p.t1);
            p.relation.clone()
        }
        QueryForm::Exists(p) => {
            f.t0 = Some(p.t0);
            f.t1 = Some(p.t1);
            p.relation.clone()
        }
        QueryForm::CountRows(p) => {
            f.t0 = Some(p.t0);
            f.t1 = Some(p.t1);
            p.relation.clone()
        }
        QueryForm::Distinct(p) => {
            f.t0 = Some(p.t0);
            f.t1 = Some(p.t1);
            p.relation.clone()
        }
        QueryForm::Catalog(p) => p.relation.clone(),
        QueryForm::Chain(_) => String::new(),
    };
    Ok((relation, f))
}

fn predicate_of(form: &QueryForm) -> Option<&Predicate> {
    match form {
        QueryForm::Scan(p) => p.predicate.as_deref(),
        QueryForm::Exists(p) => p.predicate.as_deref(),
        QueryForm::CountRows(p) => p.predicate.as_deref(),
        QueryForm::Catalog(p) => p.predicate.as_deref(),
        _ => None,
    }
}

/// Answers a query: its schema and batches.
pub fn execute(store: &Store, view: &ResolvedView, form: &QueryForm, r_max: Option<i64>, with_seq: bool) -> Result<(SchemaRef, Vec<RecordBatch>), DataError> {
    if let QueryForm::Chain(_) = form {
        // Until the option class lands (M-7) no chain is indexed.
        let schema = Arc::new(Schema::new(vec![Field::new("id", DataType::UInt64, false)]));
        return Ok((schema.clone(), vec![RecordBatch::new_empty(schema)]));
    }
    let (relation, filter) = plan(store, form, r_max, view.as_of_seq)?;
    if let QueryForm::Catalog(_) = form {
        if store.schema(&relation)?.class != "registry" {
            return Err(DataError::Malformed(format!("`catalog` reads the registry's relations; `{}` is not one", relation)));
        }
    }
    let version = view.version(store, &relation)?;
    let schema = store.manifest(&version)?.schema;
    let stored = stored_schema(&schema);
    let mut parts = store.read(&version, &filter)?;
    if let Some(p) = predicate_of(form) {
        parts = parts
            .iter()
            .map(|b| Ok(arrow::compute::filter_record_batch(b, &predicate_mask(store, &schema, b, p)?)?))
            .collect::<Result<_, DataError>>()?;
    }
    let all = rows::concat(&stored, &parts)?;
    let lay = layout(&schema);
    let rows = match form {
        QueryForm::Latest(_) if all.num_rows() > 0 => {
            // The greatest t ≤ t_max per fact key: rows are in (t, key) order,
            // so each key's last row.
            let cols: Vec<ArrayRef> = lay.key.iter().map(|&c| all.column(c).clone()).collect();
            let mut last: BTreeMap<Vec<u8>, usize> = BTreeMap::new();
            if cols.is_empty() {
                last.insert(Vec::new(), all.num_rows() - 1);
            } else {
                let conv = arrow::row::RowConverter::new(cols.iter().map(|c| arrow::row::SortField::new(c.data_type().clone())).collect())?;
                let keys = conv.convert_columns(&cols)?;
                for i in 0..all.num_rows() {
                    last.insert(keys.row(i).as_ref().to_vec(), i);
                }
            }
            let mut keep: Vec<u32> = last.values().map(|&i| i as u32).collect();
            keep.sort_unstable();
            arrow::compute::take_record_batch(&all, &arrow::array::UInt32Array::from(keep))?
        }
        _ => all,
    };
    match form {
        QueryForm::Exists(_) => {
            let schema = Arc::new(Schema::new(vec![Field::new("exists", DataType::Boolean, false)]));
            let b = RecordBatch::try_new(schema.clone(), vec![Arc::new(BooleanArray::from(vec![rows.num_rows() > 0])) as ArrayRef])?;
            Ok((schema, vec![b]))
        }
        QueryForm::CountRows(_) => {
            let schema = Arc::new(Schema::new(vec![Field::new("count", DataType::Int64, false)]));
            let b = RecordBatch::try_new(schema.clone(), vec![Arc::new(Int64Array::from(vec![rows.num_rows() as i64])) as ArrayRef])?;
            Ok((schema, vec![b]))
        }
        QueryForm::Distinct(d) => {
            let ci = schema
                .columns
                .iter()
                .position(|c| c.name == d.column)
                .ok_or_else(|| DataError::Malformed(format!("`{}` has no column `{}`", schema.name, d.column)))?;
            let ans = rows::answer(&schema, &rows, store.registry.labels(), false)?;
            let col = ans.column(ci).clone();
            let field = Field::new(&d.column, col.data_type().clone(), true);
            // Distinct values in their first-seen order, which is (t, identity) order.
            let mut seen = std::collections::BTreeSet::new();
            let mut keep = Vec::new();
            let conv = arrow::row::RowConverter::new(vec![arrow::row::SortField::new(col.data_type().clone())])?;
            let keys = conv.convert_columns(std::slice::from_ref(&col))?;
            for i in 0..col.len() {
                if seen.insert(keys.row(i).as_ref().to_vec()) {
                    keep.push(i as u32);
                }
            }
            let picked = arrow::compute::take(&col, &arrow::array::UInt32Array::from(keep), None)?;
            let schema = Arc::new(Schema::new(vec![field]));
            Ok((schema.clone(), vec![RecordBatch::try_new(schema, vec![picked])?]))
        }
        QueryForm::Scan(sc) if sc.columns.is_some() => {
            let ans = rows::answer(&schema, &rows, store.registry.labels(), with_seq)?;
            let wanted = sc.columns.clone().unwrap_or_default();
            let idx: Vec<usize> = wanted
                .iter()
                .map(|c| ans.schema().index_of(c).map_err(|_| DataError::Malformed(format!("`{}` has no column `{}`", schema.name, c))))
                .collect::<Result<_, _>>()?;
            let projected = ans.project(&idx)?;
            Ok((projected.schema(), vec![projected]))
        }
        _ => {
            let ans = rows::answer(&schema, &rows, store.registry.labels(), with_seq)?;
            Ok((ans.schema(), vec![ans]))
        }
    }
}

/// The estimate of a query (D-4.1 `explain`): rows and bytes read, the
/// partitions touched, whether a chain query found its chain indexed.
pub fn explain(store: &Store, view: &ResolvedView, form: &QueryForm) -> Result<ExplainResponse, DataError> {
    if let QueryForm::Chain(_) = form {
        return Ok(ExplainResponse {
            rows: 0,
            bytes: 0,
            partitions: 0,
            cached: false,
            unresolved: true,
        });
    }
    let (relation, f) = plan(store, form, None, view.as_of_seq)?;
    let version = view.version(store, &relation)?;
    let m = store.manifest(&version)?;
    let mut rows_est = 0u64;
    let mut parts = 0u64;
    let mut bytes = 0u64;
    for p in &m.partitions {
        if f.t0.is_some_and(|t0| p.t_max < t0) || f.t1.is_some_and(|t1| p.t_min > t1) {
            continue;
        }
        parts += 1;
        rows_est += p.rows;
        bytes += std::fs::metadata(store.root().join(&p.file)).map(|m| m.len()).unwrap_or(0);
    }
    Ok(ExplainResponse {
        rows: rows_est,
        bytes,
        partitions: parts,
        cached: false,
        unresolved: false,
    })
}
