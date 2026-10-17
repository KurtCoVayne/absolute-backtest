//! An adapter over rows held in memory that counts what it is asked: the
//! source the resolver's tests buy from, and the witness that a demand
//! with money budget 0 never opens a vendor connection (A-D22, M-6).

use std::sync::{Arc, Mutex};

use arrow::array::{AsArray, BooleanArray, RecordBatch};
use arrow::datatypes::{DataType, TimestampNanosecondType};

use super::{Adapter, FetchRequest, Fetched};
use crate::adapter::Security;
use crate::error::DataError;
use crate::resolver::catalog::Offer;

/// A mock vendor: its offers and, per relation, the rows it holds (entity
/// columns as the vendor's text keys).
pub struct Mock {
    vendor: String,
    offers: Vec<Offer>,
    securities: Vec<Security>,
    rows: Vec<(String, RecordBatch)>,
    /// Every request it was asked, in order: a connection opened is a
    /// request recorded.
    pub asked: Arc<Mutex<Vec<FetchRequest>>>,
}

impl Mock {
    pub fn new(vendor: &str, offers: Vec<Offer>, securities: Vec<Security>, rows: Vec<(String, RecordBatch)>) -> Mock {
        Mock {
            vendor: vendor.to_string(),
            offers,
            securities,
            rows,
            asked: Arc::default(),
        }
    }

    /// The requests it was asked so far.
    pub fn requests(&self) -> Vec<FetchRequest> {
        self.asked.lock().map(|a| a.clone()).unwrap_or_default()
    }
}

/// The rows of `b` whose first time column lies in `[from, to]`.
fn within(b: &RecordBatch, from: i64, to: i64) -> Result<RecordBatch, DataError> {
    let Some(col) = b.schema().fields().iter().position(|f| matches!(f.data_type(), DataType::Timestamp(_, _))) else {
        return Ok(b.clone());
    };
    let t = b.column(col).as_primitive::<TimestampNanosecondType>();
    let keep = BooleanArray::from((0..b.num_rows()).map(|i| (from..=to).contains(&t.value(i))).collect::<Vec<_>>());
    Ok(arrow::compute::filter_record_batch(b, &keep)?)
}

impl Adapter for Mock {
    fn vendor(&self) -> &str {
        &self.vendor
    }

    fn offers(&self) -> Vec<Offer> {
        self.offers.clone()
    }

    fn fetch(&self, req: &FetchRequest) -> Result<Fetched, DataError> {
        if let Ok(mut a) = self.asked.lock() {
            a.push(req.clone());
        }
        let mut relations = Vec::new();
        for r in &req.relations {
            let (_, b) = self
                .rows
                .iter()
                .find(|(name, _)| name == r)
                .ok_or_else(|| DataError::NotFound(format!("the mock vendor `{}` holds no `{}`", self.vendor, r)))?;
            relations.push((r.clone(), within(b, req.from, req.to)?));
        }
        Ok(Fetched {
            securities: self.securities.clone(),
            terms: vec![],
            relations,
            attributes: Default::default(),
        })
    }
}
