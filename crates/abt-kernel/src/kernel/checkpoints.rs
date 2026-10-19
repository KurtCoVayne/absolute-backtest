//! Checkpoints through the data layer (K-4.7): a checkpoint (Σ_t with the
//! executor's state and the stream cursor) is a cache object (D-3.5) keyed
//! by (run id, seq), its serialised form in parts of at most a MiB. Resume
//! reads it by its key and verifies (H(P), β, mode) against the run.

use std::sync::Arc;

use abt_wire::generated::abt::wire::{ColumnSpec, ColumnType, RelationSchema, Role};
use arrow::array::{Array, AsArray, BinaryArray, RecordBatch, TimestampNanosecondArray, UInt64Array};
use arrow::datatypes::{DataType, Field, Schema, TimeUnit, UInt64Type};

use super::{Checkpoint, DataClient, RunError};

/// The largest part of a checkpoint's serialised form.
const PART: usize = 1 << 20;

/// The cache key of a run's checkpoint at a stream position (K-4.7).
pub fn key(run_id: &str, seq: u64) -> String {
    let v = serde_json::json!({ "checkpoint": run_id, "seq": seq });
    abt_wire::Hash::of(abt_wire::to_canonical_json(&v).expect("a key is JSON").as_bytes()).hex()
}

fn col(name: &str, ty: ColumnType, role: Role) -> ColumnSpec {
    ColumnSpec {
        name: name.into(),
        ty,
        role,
        dimension: None,
    }
}

/// Writes a checkpoint as a cache object; returns its key.
pub fn write(client: &DataClient, cp: &Checkpoint) -> Result<String, RunError> {
    let bytes = serde_json::to_vec(cp).map_err(|e| RunError::Internal(format!("a checkpoint does not serialise: {}", e)))?;
    let parts: Vec<&[u8]> = bytes.chunks(PART).collect();
    let n = parts.len();
    let schema = RelationSchema {
        name: format!("kernel.{}.checkpoint", cp.identity.run_id),
        class: "kernel".into(),
        columns: vec![
            col("part", ColumnType::Entity, Role::Entity),
            col("t", ColumnType::Time, Role::EventTime),
            col("data", ColumnType::Bytes, Role::Value),
        ],
        calendar: None,
        constraints: None,
        attributes: None,
        schema_version: Some("1.0".into()),
    };
    let arrow = Arc::new(Schema::new(vec![
        Field::new("part", DataType::UInt64, false),
        Field::new("t", DataType::Timestamp(TimeUnit::Nanosecond, None), false),
        Field::new("data", DataType::Binary, false),
    ]));
    let batch = RecordBatch::try_new(
        arrow,
        vec![
            Arc::new(UInt64Array::from((0..n as u64).collect::<Vec<_>>())),
            Arc::new(TimestampNanosecondArray::from(vec![cp.last_bar; n])),
            Arc::new(BinaryArray::from(parts)),
        ],
    )
    .map_err(|e| RunError::Internal(format!("a checkpoint's table: {}", e)))?;
    let k = key(&cp.identity.run_id, cp.cursor);
    client.put_cache(&k, schema, &[batch])?;
    Ok(k)
}

/// The checkpoint under a key.
pub fn read(client: &DataClient, key: &str) -> Result<Checkpoint, RunError> {
    let batches = client
        .get_cache(key)?
        .ok_or_else(|| RunError::Request(format!("the data layer holds no checkpoint {} (never written, or evicted)", key)))?;
    let mut parts: Vec<(u64, Vec<u8>)> = Vec::new();
    for b in &batches {
        let (Some(p), Some(d)) = (b.column_by_name("part"), b.column_by_name("data")) else {
            return Err(RunError::Request(format!("the cache object {} is not a checkpoint", key)));
        };
        let (p, d) = (p.as_primitive::<UInt64Type>(), d.as_binary::<i32>());
        for i in 0..b.num_rows() {
            if !d.is_null(i) {
                parts.push((p.value(i), d.value(i).to_vec()));
            }
        }
    }
    parts.sort_by_key(|x| x.0);
    let bytes: Vec<u8> = parts.into_iter().flat_map(|(_, d)| d).collect();
    serde_json::from_slice(&bytes).map_err(|e| RunError::Request(format!("the checkpoint {} does not read: {}", key, e)))
}
