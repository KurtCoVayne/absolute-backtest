//! Tables on the wire (P-2.2, P-4): a batch larger than one frame.

use std::sync::Arc;

use abt_wire::frame::MAX_FRAME;
use abt_wire::table::{TableDecoder, TableEncoder};
use arrow::array::{ArrayRef, DictionaryArray, Float64Array, RecordBatch, StringArray, UInt32Array};
use arrow::datatypes::{DataType, Field, Schema, UInt32Type};

/// A batch whose IPC bytes pass a frame's 64 MiB, with a dictionary column
/// (a label column of an event batch): the decoder gets the batch back,
/// every key against its dictionary, from frames each under the limit. The
/// encoder once re-encoded such a batch as two halves after its writer had
/// counted the dictionary as sent, so the halves went without it and the
/// reader validated their keys against nothing (E-93).
#[test]
fn a_batch_larger_than_a_frame_keeps_its_dictionaries() {
    let n = 6_000_000usize;
    let labels: Vec<String> = (0..4000).map(|i| format!("L{}", i)).collect();
    let keys = UInt32Array::from((0..n).map(|i| Some(((i * 7919) % 4000) as u32)).collect::<Vec<_>>());
    let dict = DictionaryArray::<UInt32Type>::try_new(keys, Arc::new(StringArray::from(labels))).unwrap();
    let values = Float64Array::from((0..n).map(|i| i as f64 * 0.5).collect::<Vec<_>>());
    let schema = Arc::new(Schema::new(vec![
        Field::new("label", DataType::Dictionary(Box::new(DataType::UInt32), Box::new(DataType::Utf8)), true),
        Field::new("x", DataType::Float64, false),
    ]));
    let batch = RecordBatch::try_new(schema.clone(), vec![Arc::new(dict) as ArrayRef, Arc::new(values)]).unwrap();
    let (mut enc, head) = TableEncoder::new(&schema).unwrap();
    let payloads = enc.batch(&batch).unwrap();
    assert!(payloads.len() > 1, "the batch passes one frame");
    assert!(payloads.iter().all(|p| p.len() < MAX_FRAME), "every frame under the limit");
    let mut dec = TableDecoder::new();
    let mut got = dec.feed(&head).unwrap();
    for p in &payloads {
        got.extend(dec.feed(p).unwrap());
    }
    assert_eq!(got.iter().map(|b| b.num_rows()).sum::<usize>(), n);
    let back = arrow::compute::concat_batches(&got[0].schema(), &got).unwrap();
    let back_labels = arrow::compute::cast(back.column(0), &DataType::Utf8).unwrap();
    let want_labels = arrow::compute::cast(batch.column(0), &DataType::Utf8).unwrap();
    assert_eq!(back_labels.as_ref(), want_labels.as_ref(), "the labels, key by key against the dictionary");
    assert_eq!(back.column(1).as_ref(), batch.column(1).as_ref());
}
