//! The query layer's reader of the data layer (Q-1, A-D20): it reads the
//! store through the data service like every other client, and never
//! stores a byte itself.

use abt_wire::generated::abt::wire::{Message, PinRequest, PutRequest, QueryForm, QueryRequest, RelationSchema, Scan, SchemaRequest, View};
use abt_wire::rpc::{Client, WireError};
use abt_wire::transport::Connection;
use abt_wire::Layer;
use arrow::record_batch::RecordBatch;

/// What reading the data layer can fail with.
#[derive(Debug)]
pub enum Error {
    /// The data service refused or the transport failed.
    Wire(String),
    /// A relation the reader needs is not in the store.
    Missing(String),
    /// A relation is in the store with another shape than the reader's.
    Malformed(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Wire(m) => write!(f, "the data service: {}", m),
            Error::Missing(m) => write!(f, "{}", m),
            Error::Malformed(m) => write!(f, "{}", m),
        }
    }
}

impl std::error::Error for Error {}

fn wire(e: WireError) -> Error {
    Error::Wire(e.to_string())
}

/// A client of the data service.
pub struct Data {
    client: Client,
}

impl Data {
    pub fn connect(conn: Connection) -> Result<Data, Error> {
        Ok(Data {
            client: Client::connect(conn, Layer::Query, "abt-query").map_err(wire)?,
        })
    }

    /// The schemas the registry holds, of a class or namespace.
    pub fn schemas(&self, class: Option<&str>) -> Result<Vec<RelationSchema>, Error> {
        match self
            .client
            .request(Message::SchemaRequest(Box::new(SchemaRequest {
                class: class.map(str::to_string),
                relation: None,
            })))
            .map_err(wire)?
        {
            Message::SchemaResponse(r) => Ok(r.schemas.unwrap_or_default()),
            _ => Err(Error::Wire("a schema request answered with another message".into())),
        }
    }

    /// The current version of every relation whose name starts with
    /// `prefix`, by name (what a metric is recorded with, Q-3.1).
    pub fn versions(&self, prefix: &str) -> Result<std::collections::BTreeMap<String, String>, Error> {
        use arrow::array::AsArray;
        let (_, batches) = self
            .client
            .request_table(Message::StatsRequest(Box::new(abt_wire::generated::abt::wire::StatsRequest { relation: None })))
            .map_err(wire)?;
        let mut out = std::collections::BTreeMap::new();
        for b in &batches {
            let (Some(r), Some(v), Some(c)) = (b.column_by_name("relation"), b.column_by_name("version"), b.column_by_name("current")) else {
                return Err(Error::Malformed("the data layer's statistics lack relation, version or current".into()));
            };
            let (r, v, c) = (r.as_string::<i32>(), v.as_string::<i32>(), c.as_boolean());
            for i in 0..b.num_rows() {
                if c.value(i) && r.value(i).starts_with(prefix) {
                    out.insert(r.value(i).to_string(), v.value(i).to_string());
                }
            }
        }
        Ok(out)
    }

    /// Rows put under the query layer's namespace (D-3.6): added to the
    /// relation's current version, or, with `replace`, its next version
    /// whole (E-56). The version written.
    pub fn put(&self, schema: RelationSchema, batch: &RecordBatch, replace: bool) -> Result<String, Error> {
        let relation = schema.name.clone();
        let mut reply = self
            .client
            .call_with_table(
                Message::PutRequest(Box::new(PutRequest {
                    relation,
                    schema: Box::new(schema),
                    attributes: None,
                    cache_key: None,
                    replace,
                })),
                &batch.schema(),
                std::slice::from_ref(batch),
            )
            .map_err(wire)?;
        match reply.message().map_err(wire)? {
            Message::PutResponse(p) => Ok(p.version),
            _ => Err(Error::Wire("a put answered with another message".into())),
        }
    }

    /// Pins (or unpins) the versions of a binding (Q-2.6).
    pub fn pin(&self, binding: &str, pin: bool) -> Result<(), Error> {
        let mut reply = self
            .client
            .call(Message::PinRequest(Box::new(PinRequest {
                version: None,
                binding: Some(binding.to_string()),
                pin,
            })))
            .map_err(wire)?;
        reply.message().map(|_| ()).map_err(wire)
    }

    /// The names of the relations whose name starts with `prefix` and ends
    /// with `suffix`, with their current versions.
    pub fn relations(&self, prefix: &str, suffix: &str) -> Result<Vec<(String, String)>, Error> {
        Ok(self.versions(prefix)?.into_iter().filter(|(r, _)| r.ends_with(suffix)).collect())
    }

    /// Every row of a relation's current version, in the store's order (t,
    /// then identity); none when it has no row.
    pub fn scan(&self, relation: &str) -> Result<Vec<RecordBatch>, Error> {
        let form = QueryForm::Scan(Box::new(Scan {
            relation: relation.to_string(),
            t0: i64::MIN,
            t1: i64::MAX,
            predicate: None,
            columns: None,
        }));
        let view = View {
            versions: None,
            binding: None,
            as_of_seq: None,
        };
        self.client
            .request_table(Message::QueryRequest(Box::new(QueryRequest {
                view: Some(Box::new(view)),
                form,
                receipt_filter: None,
                with_seq: false,
                explain: false,
                ticks: false,
            })))
            .map(|(_, b)| b)
            .map_err(|e| match e {
                WireError::Remote(r) if matches!(r.code, abt_wire::ErrorCode::NotFound | abt_wire::ErrorCode::UnknownRelation) => Error::Missing(format!("the store has no relation {}", relation)),
                e => wire(e),
            })
    }
}
