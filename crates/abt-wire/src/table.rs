//! Tables on the wire (P-2.2, P-4): a table follows its control response as
//! one `schema` frame (an Arrow IPC schema message), `batch` frames (the
//! IPC messages of the record batches, each batch's dictionaries first) and
//! an `end` frame. A batch whose IPC messages would pass 64 MiB spans
//! several `batch` frames, cut at the frame's limit.

use arrow::array::RecordBatch;
use arrow::buffer::Buffer;
use arrow::datatypes::SchemaRef;
use arrow::error::ArrowError;
use arrow::ipc::reader::StreamDecoder;
use arrow::ipc::writer::StreamWriter;

use crate::frame::{HEADER_AFTER_LENGTH, MAX_FRAME};

/// The schema version every table carries in its IPC metadata (P-4).
pub const TABLE_SCHEMA_VERSION: &str = "1.0";

/// The payload of a batch frame may use what a frame allows beside its header.
const MAX_PAYLOAD: usize = MAX_FRAME - 4 - HEADER_AFTER_LENGTH;

/// Encodes a table's schema and batches into frame payloads.
pub struct TableEncoder {
    writer: StreamWriter<Vec<u8>>,
}

impl TableEncoder {
    /// The encoder and the schema frame's payload.
    pub fn new(schema: &SchemaRef) -> Result<(TableEncoder, Vec<u8>), ArrowError> {
        let mut meta = schema.metadata().clone();
        meta.insert("abt.schema".to_string(), TABLE_SCHEMA_VERSION.to_string());
        let schema = arrow::datatypes::Schema::new_with_metadata(schema.fields().clone(), meta);
        let mut writer = StreamWriter::try_new(Vec::new(), &schema)?;
        let head = std::mem::take(writer.get_mut());
        Ok((TableEncoder { writer }, head))
    }

    /// The payloads of one batch's frames: one, or several when the batch's
    /// IPC messages are larger than a frame allows. The bytes are cut, not
    /// the batch: the writer has sent the batch's dictionaries once it has
    /// written it (a re-encoded half would go without them, E-93), and the
    /// decoder reads a byte stream cut anywhere.
    pub fn batch(&mut self, batch: &RecordBatch) -> Result<Vec<Vec<u8>>, ArrowError> {
        let schema = self.writer_schema(batch);
        let batch = if schema != batch.schema() { batch.clone().with_schema(schema)? } else { batch.clone() };
        self.writer.write(&batch)?;
        let bytes = std::mem::take(self.writer.get_mut());
        if bytes.len() <= MAX_PAYLOAD {
            return Ok(vec![bytes]);
        }
        Ok(bytes.chunks(MAX_PAYLOAD).map(|c| c.to_vec()).collect())
    }

    fn writer_schema(&self, batch: &RecordBatch) -> SchemaRef {
        // The writer was opened with the table's schema plus the version key;
        // a batch carries the schema without it.
        let s = batch.schema();
        let mut meta = s.metadata().clone();
        meta.insert("abt.schema".to_string(), TABLE_SCHEMA_VERSION.to_string());
        std::sync::Arc::new(arrow::datatypes::Schema::new_with_metadata(s.fields().clone(), meta))
    }
}

/// Decodes a table's frame payloads back into batches.
#[derive(Default)]
pub struct TableDecoder {
    decoder: StreamDecoder,
    schema: Option<SchemaRef>,
}

impl TableDecoder {
    pub fn new() -> TableDecoder {
        TableDecoder::default()
    }

    /// Feeds one frame's payload; the batches it completed.
    pub fn feed(&mut self, payload: &[u8]) -> Result<Vec<RecordBatch>, ArrowError> {
        let mut buf = Buffer::from(payload.to_vec());
        let mut out = Vec::new();
        while !buf.is_empty() {
            match self.decoder.decode(&mut buf)? {
                Some(b) => {
                    self.check_version(&b.schema())?;
                    out.push(b);
                }
                None => {
                    if let Some(s) = self.decoder.schema() {
                        self.check_version(&s)?;
                        self.schema = Some(s);
                    }
                    if buf.is_empty() {
                        break;
                    }
                }
            }
        }
        if self.schema.is_none() {
            self.schema = self.decoder.schema();
        }
        Ok(out)
    }

    /// The table's schema, once its schema frame was fed.
    pub fn schema(&self) -> Option<SchemaRef> {
        self.schema.clone().or_else(|| self.decoder.schema())
    }

    /// A reader refuses a major version it does not know (P-7).
    fn check_version(&self, s: &SchemaRef) -> Result<(), ArrowError> {
        match s.metadata().get("abt.schema") {
            Some(v) if v.split('.').next() != TABLE_SCHEMA_VERSION.split('.').next() => Err(ArrowError::SchemaError(format!("table schema version {} is not {}", v, TABLE_SCHEMA_VERSION))),
            _ => Ok(()),
        }
    }
}
