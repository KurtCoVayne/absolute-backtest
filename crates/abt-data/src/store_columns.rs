//! Reads of a version that take the columns a question needs and nothing
//! else (E-138): a child of `store`, as `store_hot` is. A kernel's bind
//! asks every relation it binds for the distinct times and labels of its
//! whole history, and scans a membership relation for the instruments of
//! its lists; read as rows, each question decoded every column of every
//! row (the binding was 55.6 s of a 381 s run on a store whose relations
//! hold up to 15.69 million rows). Here a Parquet partition gives the
//! column chunks asked for alone, a row group whose statistics say it
//! holds one label is not decoded, and a hot version answers its times
//! from its presence and its appended labels without a block of values.
//! And a bound below a number column's values is read from the row groups'
//! statistics or the hot blocks' headers (`least_value`, E-139): a
//! comparison no value can pass, as the preconditions' prices at or below
//! zero, is answered with no row read.
//!
//! Each read answers what the row read answers, in its order, or `None`
//! where it could not be sure to (a receipt or sequence cut, partitions
//! that overlap in time, a file out of order): the caller then reads the
//! rows. `tests/column_reads.rs` holds them equal.

use std::fs;

use abt_wire::generated::abt::wire::ColumnType;
use arrow::array::{Array, ArrayRef, AsArray, BooleanArray, RecordBatch};
use arrow::datatypes::{DataType, Schema, SchemaRef, TimestampNanosecondType, UInt32Type, UInt64Type};
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use parquet::arrow::ProjectionMask;
use parquet::file::metadata::RowGroupMetaData;
use parquet::file::statistics::Statistics;

use super::{latest_revisions, read_index_rows, selection, Manifest, PartitionRef, RowFilter, Store};
use crate::error::DataError;
use crate::rows::{self, layout, stored_schema, Layout};

type Reader = ParquetRecordBatchReaderBuilder<fs::File>;

/// Whether a filter cuts by receipt, by sequence or by identity: what a
/// label or a time alone cannot answer.
fn cuts(f: &RowFilter) -> bool {
    f.identity.is_some() || f.r_max.is_some() || f.as_of_seq.is_some()
}

/// The partitions of a Parquet version a window overlaps, in time order,
/// when no two of them overlap in time: a read of one's span is then that
/// partition's rows and no other's. `None` when two overlap (a relation
/// whose partition calendar changed between writes).
fn disjoint<'m>(m: &'m Manifest, f: &RowFilter) -> Option<Vec<&'m PartitionRef>> {
    let mut parts: Vec<&PartitionRef> = m.partitions.iter().filter(|p| !(f.t0.is_some_and(|t0| p.t_max < t0) || f.t1.is_some_and(|t1| p.t_min > t1))).collect();
    parts.sort_by_key(|p| (p.t_min, p.t_max));
    parts.windows(2).all(|w| w[0].t_max < w[1].t_min).then_some(parts)
}

/// Whether a window holds every row of a partition.
fn holds(f: &RowFilter, p: &PartitionRef) -> bool {
    f.t0.is_none_or(|t0| p.t_min >= t0) && f.t1.is_none_or(|t1| p.t_max <= t1)
}

/// The columns at `columns` (positions of the stored layout) of a file.
fn project(reader: Reader, columns: &[usize]) -> Reader {
    let mask = ProjectionMask::roots(reader.parquet_schema(), columns.iter().copied());
    reader.with_projection(mask)
}

/// The least and greatest event time of a row group, when its statistics
/// state them exactly.
fn time_span(g: &RowGroupMetaData, column: usize) -> Option<(i64, i64)> {
    match g.column(column).statistics()? {
        Statistics::Int64(s) if s.min_is_exact() && s.max_is_exact() => Some((*s.min_opt()?, *s.max_opt()?)),
        _ => None,
    }
}

/// The one label code every row of a row group carries, when its statistics
/// say so: no null, and a least value equal to the greatest.
fn one_code(g: &RowGroupMetaData, column: usize) -> Option<u32> {
    match g.column(column).statistics()? {
        Statistics::Int32(s) if s.min_is_exact() && s.max_is_exact() && s.null_count_opt() == Some(0) && g.num_rows() > 0 => {
            let (lo, hi) = (*s.min_opt()?, *s.max_opt()?);
            (lo == hi).then_some(lo as u32)
        }
        _ => None,
    }
}

/// The distinct values of a label column in the order they are first met.
#[derive(Default)]
struct FirstSeen {
    codes: Vec<bool>,
    null: bool,
    order: Vec<Option<u32>>,
}

impl FirstSeen {
    fn has(&self, code: u32) -> bool {
        self.codes.get(code as usize).copied().unwrap_or(false)
    }

    fn note(&mut self, value: Option<u32>) {
        match value {
            Some(code) => {
                if self.codes.len() <= code as usize {
                    self.codes.resize(code as usize + 1, false);
                }
                if !self.codes[code as usize] {
                    self.codes[code as usize] = true;
                    self.order.push(Some(code));
                }
            }
            None if !self.null => {
                self.null = true;
                self.order.push(None);
            }
            None => {}
        }
    }
}

/// Whether rows `i` and `i + 1` of a column hold the same value (two nulls
/// are the same), for the types a fact's time and key are stored as; `None`
/// for another type.
fn same_as_next(col: &ArrayRef) -> Option<Box<dyn Fn(usize) -> bool + '_>> {
    fn of<T: arrow::datatypes::ArrowPrimitiveType>(col: &ArrayRef) -> Box<dyn Fn(usize) -> bool + '_> {
        let a = col.as_primitive::<T>();
        if a.null_count() == 0 {
            let v = a.values();
            Box::new(move |i| v[i] == v[i + 1])
        } else {
            Box::new(move |i| a.is_null(i) == a.is_null(i + 1) && (a.is_null(i) || a.value(i) == a.value(i + 1)))
        }
    }
    match col.data_type() {
        DataType::UInt64 => Some(of::<UInt64Type>(col)),
        DataType::UInt32 => Some(of::<UInt32Type>(col)),
        DataType::Timestamp(arrow::datatypes::TimeUnit::Nanosecond, _) => Some(of::<TimestampNanosecondType>(col)),
        _ => None,
    }
}

impl Store {
    fn partition_reader(&self, p: &PartitionRef) -> Result<Reader, DataError> {
        Ok(ParquetRecordBatchReaderBuilder::try_new(fs::File::open(self.root.join(&p.file))?)?)
    }

    /// The distinct event times of the rows a filter admits, ascending:
    /// what `distinct` of the event-time column answers, a time once and in
    /// time order. A row's time needs no other column of it: every (time,
    /// key) the version holds has a latest revision. `None` under a cut by
    /// receipt, sequence or identity, where a time's rows may all be cut.
    pub fn distinct_times(&self, version: &str, f: &RowFilter) -> Result<Option<Vec<i64>>, DataError> {
        if cuts(f) {
            return Ok(None);
        }
        let m = self.manifest(version)?;
        if m.hot {
            return Ok(Some(self.hot(version)?.distinct_labels(f.t0, f.t1)?));
        }
        let Some(parts) = disjoint(&m, f) else { return Ok(None) };
        let t = layout(&m.schema).t;
        let mut out: Vec<i64> = Vec::new();
        for p in parts {
            let reader = self.partition_reader(p)?;
            // A row group whose times all lie outside the window is not
            // read: it holds no row the window admits.
            let groups: Vec<usize> = reader
                .metadata()
                .row_groups()
                .iter()
                .enumerate()
                .filter(|(_, g)| !time_span(g, t).is_some_and(|(lo, hi)| f.t0.is_some_and(|t0| hi < t0) || f.t1.is_some_and(|t1| lo > t1)))
                .map(|(i, _)| i)
                .collect();
            let mut last: Option<i64> = None;
            for b in project(reader, &[t]).with_row_groups(groups).with_batch_size(super::ROW_GROUP).build()? {
                let b = b?;
                let times = b.column(0).as_primitive::<TimestampNanosecondType>();
                if times.null_count() > 0 {
                    return Ok(None);
                }
                for &x in times.values().iter() {
                    if f.t0.is_some_and(|t0| x < t0) || f.t1.is_some_and(|t1| x > t1) {
                        continue;
                    }
                    match last {
                        Some(l) if x == l => {}
                        // A file out of time order: the order of the row
                        // read is the file's, not time's.
                        Some(l) if x < l => return Ok(None),
                        _ => {
                            out.push(x);
                            last = Some(x);
                        }
                    }
                }
            }
        }
        // Partitions in time order that do not overlap give ascending
        // times; were a manifest's spans wrong, the row read answers.
        Ok(out.windows(2).all(|w| w[0] < w[1]).then_some(out))
    }

    /// The distinct codes (a null among them, when a row has one) of a
    /// label column that is part of the fact's key, in the order the row
    /// read first meets them: what `distinct` of the column answers. A
    /// label of the key needs no other column: the revisions of a fact
    /// share it, so the latest one is met where the first is. `None` for a
    /// hot version, a label that is a value (a revision may change it), a
    /// cut by receipt, sequence or identity, or partitions that overlap.
    pub fn distinct_codes(&self, version: &str, f: &RowFilter, column: usize) -> Result<Option<Vec<Option<u32>>>, DataError> {
        if cuts(f) {
            return Ok(None);
        }
        let m = self.manifest(version)?;
        let lay = layout(&m.schema);
        if m.hot || m.schema.columns.get(column).is_none_or(|c| c.ty != ColumnType::Label) || !lay.key.contains(&column) {
            return Ok(None);
        }
        let Some(parts) = disjoint(&m, f) else { return Ok(None) };
        let mut seen = FirstSeen::default();
        for p in parts {
            let reader = self.partition_reader(p)?;
            if !holds(f, p) {
                // The window cuts the partition: each row's time says
                // whether its label counts.
                let (ti, ci) = if lay.t < column { (0, 1) } else { (1, 0) };
                let mut both = [lay.t, column];
                both.sort_unstable();
                for b in project(reader, &both).with_batch_size(super::ROW_GROUP).build()? {
                    let b = b?;
                    let times = b.column(ti).as_primitive::<TimestampNanosecondType>();
                    let codes = b.column(ci).as_primitive::<UInt32Type>();
                    for (x, code) in times.values().iter().zip(codes.iter()) {
                        if f.t0.is_none_or(|t0| *x >= t0) && f.t1.is_none_or(|t1| *x <= t1) {
                            seen.note(code);
                        }
                    }
                }
                continue;
            }
            // A row group holding one label, by its statistics, is not
            // decoded; the others are, and both are met in the file's order.
            let plan: Vec<(Option<u32>, usize)> = reader.metadata().row_groups().iter().map(|g| (one_code(g, column), g.num_rows() as usize)).collect();
            if plan.iter().all(|(one, _)| one.is_some_and(|c| seen.has(c))) {
                continue;
            }
            let groups: Vec<usize> = plan.iter().enumerate().filter(|(_, (one, _))| one.is_none()).map(|(i, _)| i).collect();
            let decoded: Vec<RecordBatch> = if groups.is_empty() {
                Vec::new()
            } else {
                project(reader, &[column])
                    .with_row_groups(groups)
                    .with_batch_size(super::ROW_GROUP)
                    .build()?
                    .collect::<Result<_, _>>()?
            };
            let mut codes = decoded.iter().flat_map(|b| b.column(0).as_primitive::<UInt32Type>().iter());
            for (one, rows) in plan {
                match one {
                    Some(code) => seen.note(Some(code)),
                    None => {
                        for _ in 0..rows {
                            let Some(code) = codes.next() else {
                                return Err(DataError::Order(format!("{}: a row group shorter than its metadata says", p.file)));
                            };
                            seen.note(code);
                        }
                    }
                }
            }
        }
        Ok(Some(seen.order))
    }

    /// A number no value of a number column is below, over every row the
    /// version holds in the partitions a window overlaps, superseded rows
    /// among them, with no row read: a Parquet version's from the least
    /// value each row group's statistics state for the column (a writer
    /// leaves a value that is not a number out of them, and a least value
    /// that is not exact is still below every value), a hot version's from
    /// its blocks' headers (`HotVersion::least_value`, over the whole
    /// version). Infinity where there is no value; `None` for a column of
    /// another type and where the store cannot tell (a row group without
    /// the statistic, a value that is not a number in a hot block).
    pub fn least_value(&self, version: &str, f: &RowFilter, column: usize) -> Result<Option<f64>, DataError> {
        let m = self.manifest(version)?;
        let lay = layout(&m.schema);
        let value = |i: usize| i != lay.t && Some(i) != lay.r && !lay.identity.contains(&i);
        if m.schema.columns.get(column).is_none_or(|c| c.ty != ColumnType::Num) || !value(column) {
            return Ok(None);
        }
        if m.hot {
            // The column's place among the value columns, as the blocks
            // number them.
            return self.hot(version)?.least_value((0..column).filter(|&i| value(i)).count());
        }
        let mut least = f64::INFINITY;
        for p in &m.partitions {
            if f.t0.is_some_and(|t0| p.t_max < t0) || f.t1.is_some_and(|t1| p.t_min > t1) {
                continue;
            }
            for g in self.partition_reader(p)?.metadata().row_groups() {
                if g.num_rows() == 0 {
                    continue;
                }
                // The file's column at the schema's position, when it is
                // the schema's column.
                let Some(chunk) = g.columns().get(column).filter(|c| c.column_descr().name() == m.schema.columns[column].name) else {
                    return Ok(None);
                };
                match chunk.statistics() {
                    Some(Statistics::Double(s)) => match s.min_opt() {
                        Some(x) if !x.is_nan() => least = least.min(*x),
                        // Every row of the group is null: it holds no value.
                        None if s.null_count_opt() == Some(g.num_rows() as u64) => {}
                        _ => return Ok(None),
                    },
                    _ => return Ok(None),
                }
            }
        }
        Ok(Some(least))
    }

    /// The latest revision of each fact a filter admits, as the columns at
    /// `columns` alone (positions of the stored layout, ascending, the
    /// event time and the key among them) under their stored fields: what
    /// `read` gives a `latest_only` filter, less the other columns, in the
    /// same order. `None` for a hot version, for a filter that keeps every
    /// revision, or under a cut by receipt or sequence, whose columns the
    /// read would need.
    pub fn read_columns(&self, version: &str, f: &RowFilter, columns: &[usize]) -> Result<Option<RecordBatch>, DataError> {
        let m = self.manifest(version)?;
        let lay = layout(&m.schema);
        let at = |c: usize| columns.iter().position(|&x| x == c);
        let key: Option<Vec<usize>> = lay.key.iter().map(|&c| at(c)).collect();
        let (Some(t), Some(key)) = (at(lay.t), key) else { return Ok(None) };
        if m.hot || !f.latest_only || f.r_max.is_some() || f.as_of_seq.is_some() || !columns.windows(2).all(|w| w[0] < w[1]) {
            return Ok(None);
        }
        let stored = stored_schema(&m.schema);
        let narrow: SchemaRef = std::sync::Arc::new(Schema::new(columns.iter().map(|&c| stored.field(c).clone()).collect::<Vec<_>>()));
        // The identity a filter names is read through the partition's index
        // and then checked on the rows, as the row read does: with more
        // than one entity column, or another kind of one, it keeps no row.
        let identity: Option<(usize, bool)> = match lay.identity.as_slice() {
            [i] if m.schema.columns[*i].ty == ColumnType::Entity => at(*i).map(|c| (c, true)),
            [i] if m.schema.columns[*i].ty == ColumnType::Label => at(*i).map(|c| (c, false)),
            _ => None,
        };
        let want: Option<std::collections::BTreeSet<u64>> = f.identity.as_ref().map(|v| v.iter().copied().collect());
        let mut out: Vec<RecordBatch> = Vec::new();
        for p in &m.partitions {
            if f.t0.is_some_and(|t0| p.t_max < t0) || f.t1.is_some_and(|t1| p.t_min > t1) {
                continue;
            }
            let mut reader = project(self.partition_reader(p)?, columns);
            if let Some(ids) = &f.identity {
                let ix = self.root.join(&p.file).with_extension("idx");
                if ix.exists() {
                    reader = reader.with_row_selection(selection(&read_index_rows(&fs::read(&ix)?, ids)?, p.rows as usize));
                }
            }
            for b in reader.with_batch_size(super::ROW_GROUP).build()? {
                let b = RecordBatch::try_new(narrow.clone(), b?.columns().to_vec())?;
                if want.is_none() && holds(f, p) {
                    out.push(b);
                    continue;
                }
                let times = rows::times(&b, t);
                let ids = identity.filter(|(_, entity)| *entity).map(|(c, _)| b.column(c).as_primitive::<UInt64Type>());
                let labels = identity.filter(|(_, entity)| !*entity).map(|(c, _)| b.column(c).as_primitive::<UInt32Type>());
                let keep: BooleanArray = (0..b.num_rows())
                    .map(|i| {
                        let x = times.value(i);
                        let mut ok = f.t0.is_none_or(|t0| x >= t0) && f.t1.is_none_or(|t1| x <= t1);
                        if let Some(w) = &want {
                            let id = ids.map(|a| a.value(i)).or_else(|| labels.map(|a| a.value(i) as u64));
                            ok &= id.is_some_and(|id| w.contains(&id));
                        }
                        Some(ok)
                    })
                    .collect();
                let kept = arrow::compute::filter_record_batch(&b, &keep)?;
                if kept.num_rows() > 0 {
                    out.push(kept);
                }
            }
        }
        let all = rows::concat(&narrow, &out)?;
        // The rows are in (time, key, receipt) order: a fact's revisions
        // are neighbours and the last of them is the latest. Where the time
        // and the key are integers, neighbours are compared as such, and a
        // version without a revision is answered as it was read.
        let n = all.num_rows();
        let superseded: Option<Option<BooleanArray>> = {
            let same: Option<Vec<_>> = std::iter::once(t).chain(key.iter().copied()).map(|c| same_as_next(all.column(c))).collect();
            same.map(|same| {
                let revised = |i: usize| same.iter().all(|s| s(i));
                (n > 1 && (0..n - 1).any(&revised)).then(|| (0..n).map(|i| Some(i + 1 == n || !revised(i))).collect())
            })
        };
        match superseded {
            Some(None) => Ok(Some(all)),
            Some(Some(keep)) => Ok(Some(arrow::compute::filter_record_batch(&all, &keep)?)),
            None => {
                let narrow_layout = Layout {
                    t,
                    r: None,
                    identity: Vec::new(),
                    key,
                    seq: columns.len(),
                };
                Ok(Some(latest_revisions(&narrow_layout, &all)?))
            }
        }
    }
}
