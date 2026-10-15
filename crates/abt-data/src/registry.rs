//! The registry (D-2.2 to D-2.6): schemas by (class, name), the label
//! dictionary, the instrument id counter, the current version of each
//! (relation, source), and the cache index. It persists as JSON beside the
//! store; the identity relations themselves (`instrument`, `vendor_id`,
//! `ticker`, `session`, `calendar_label`, ...) are relations of the store in
//! the `registry` class, read with the `catalog` query form.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use abt_wire::generated::abt::wire::{ColumnSpec, ColumnType, RelationSchema, Role};
use serde::{Deserialize, Serialize};

use crate::catalog;
use crate::error::DataError;

/// The registry's identity relations (D-2.4), with their schemas.
pub fn registry_schemas() -> Vec<RelationSchema> {
    fn col(name: &str, ty: ColumnType, role: Role) -> ColumnSpec {
        ColumnSpec {
            name: name.to_string(),
            ty,
            role,
            dimension: None,
        }
    }
    let rs = |name: &str, columns: Vec<ColumnSpec>| RelationSchema {
        name: name.to_string(),
        class: "registry".to_string(),
        columns,
        calendar: None,
        constraints: None,
        attributes: None,
        schema_version: Some(catalog::SCHEMA_VERSION.to_string()),
    };
    use ColumnType::*;
    use Role::{Entity as E, EventTime as T, Label as L, ReceiptTime as R, Value as V};
    vec![
        rs(
            "instrument",
            vec![
                col("id", Entity, E),
                col("from", Time, T),
                col("to", Time, V),
                col("class", Label, L),
                col("exchange", Label, L),
                col("currency", Label, L),
                col("r", Time, R),
            ],
        ),
        rs(
            "vendor_id",
            vec![
                col("id", Entity, E),
                col("from", Time, T),
                col("to", Time, V),
                col("vendor", Label, L),
                col("key", Label, L),
                col("r", Time, R),
            ],
        ),
        rs("ticker", vec![col("id", Entity, E), col("t", Time, T), col("symbol", Label, L), col("r", Time, R)]),
        rs(
            "future_spec",
            vec![
                col("id", Entity, E),
                col("first_trade", Time, T),
                col("root", Label, L),
                col("expiry", Time, V),
                col("last_trade", Time, V),
                col("point_value", Num, V),
                col("tick", Num, V),
                col("settlement", Label, L),
                col("r", Time, R),
            ],
        ),
        rs(
            "continuous",
            vec![
                col("root_id", Entity, E),
                col("t", Time, T),
                col("contract_id", Entity, V),
                col("adjustment", Num, V),
                col("r", Time, R),
            ],
        ),
        rs(
            "option_spec",
            vec![
                col("id", Entity, E),
                col("listed", Time, T),
                col("underlying", Entity, V),
                col("right", Label, L),
                col("strike", Num, V),
                col("expiry", Time, V),
                col("style", Label, L),
                col("settlement", Label, L),
                col("multiplier", Num, V),
                col("occ", Label, L),
                col("r", Time, R),
            ],
        ),
        rs("chain_index", vec![col("underlying", Entity, E), col("t", Time, T), col("id", Entity, V), col("r", Time, R)]),
        rs("calendar_label", vec![col("calendar", Label, E), col("label", Time, T), col("r", Time, R)]),
    ]
}

/// A version of a relation, as the registry records it (D-2.6).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct VersionRecord {
    pub relation: String,
    pub version: String,
    pub source: String,
    pub rows: u64,
    pub from: Option<i64>,
    pub to: Option<i64>,
    pub attributes: BTreeMap<String, String>,
    /// The log a log-prefix version reads, and its last sequence number;
    /// `None` for a sealed version.
    pub log_prefix: Option<(String, u64)>,
}

/// What persists of the registry.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct State {
    /// Label codes, in code order (D-2.5: append-only, never reused).
    labels: Vec<String>,
    /// The next instrument id (D-2.4: a counter, never reused).
    next_id: u64,
    /// Schemas by relation name (the class is a field of the schema).
    schemas: BTreeMap<String, Vec<serde_json::Value>>,
    /// Every version by id.
    versions: BTreeMap<String, VersionRecord>,
    /// The current version of each (relation, source), D-2.6.
    current: BTreeMap<String, BTreeMap<String, String>>,
    /// Cache objects: the canonical request's hash to (relation, version), D-3.5.
    cache: BTreeMap<String, (String, String)>,
    /// Pinned versions, with the bindings or plans that pin them (D-3.4).
    pins: BTreeMap<String, Vec<String>>,
}

/// The registry of a store.
pub struct Registry {
    path: PathBuf,
    state: State,
    label_index: BTreeMap<String, u32>,
}

impl Registry {
    /// Opens the registry under `dir`, creating it with the built-in classes'
    /// schemas and the identity relations' when it does not exist.
    pub fn open(dir: &Path) -> Result<Registry, DataError> {
        fs::create_dir_all(dir)?;
        let path = dir.join("registry.json");
        let state: State = if path.exists() {
            serde_json::from_slice(&fs::read(&path)?)?
        } else {
            State { next_id: 1, ..State::default() }
        };
        let label_index = state.labels.iter().enumerate().map(|(i, l)| (l.clone(), i as u32)).collect();
        let mut reg = Registry { path, state, label_index };
        if reg.state.schemas.is_empty() {
            for s in catalog::builtin_schemas().into_iter().chain(registry_schemas()) {
                reg.register_schema(&s)?;
            }
            reg.save()?;
        }
        Ok(reg)
    }

    /// Writes the registry (a whole-file replace through a temporary file).
    pub fn save(&self) -> Result<(), DataError> {
        let tmp = self.path.with_extension("json.tmp");
        fs::write(&tmp, serde_json::to_vec_pretty(&self.state)?)?;
        fs::rename(&tmp, &self.path)?;
        Ok(())
    }

    // ------------------------------------------------------------- schemas

    /// Registers a schema; a schema already registered under the name with
    /// different columns is kept beside it as a later schema version (P-7).
    pub fn register_schema(&mut self, s: &RelationSchema) -> Result<(), DataError> {
        validate_schema(s)?;
        let json = serde_json::to_value(s)?;
        let list = self.state.schemas.entry(s.name.clone()).or_default();
        if list.last() != Some(&json) {
            list.push(json);
        }
        Ok(())
    }

    /// The latest schema of a relation.
    pub fn schema(&self, relation: &str) -> Option<RelationSchema> {
        self.state.schemas.get(relation).and_then(|l| l.last()).and_then(|j| serde_json::from_value(j.clone()).ok())
    }

    /// Every relation's latest schema, optionally of one class.
    pub fn schemas(&self, class: Option<&str>) -> Vec<RelationSchema> {
        self.state.schemas.keys().filter_map(|n| self.schema(n)).filter(|s| class.is_none_or(|c| s.class == c)).collect()
    }

    // -------------------------------------------------------------- labels

    /// The code of a label, interning it.
    pub fn label(&mut self, text: &str) -> u32 {
        if let Some(&c) = self.label_index.get(text) {
            return c;
        }
        let c = self.state.labels.len() as u32;
        self.state.labels.push(text.to_string());
        self.label_index.insert(text.to_string(), c);
        c
    }

    /// The code of a label already interned.
    pub fn label_code(&self, text: &str) -> Option<u32> {
        self.label_index.get(text).copied()
    }

    pub fn label_text(&self, code: u32) -> Option<&str> {
        self.state.labels.get(code as usize).map(|s| s.as_str())
    }

    /// The dictionary: every label in code order.
    pub fn labels(&self) -> &[String] {
        &self.state.labels
    }

    // ---------------------------------------------------------------- ids

    /// A fresh instrument id (D-2.4: never reused).
    pub fn new_id(&mut self) -> u64 {
        let id = self.state.next_id;
        self.state.next_id += 1;
        id
    }

    // ----------------------------------------------------------- versions

    pub fn record_version(&mut self, v: VersionRecord) {
        self.state.versions.insert(v.version.clone(), v);
    }

    pub fn version(&self, id: &str) -> Option<&VersionRecord> {
        self.state.versions.get(id)
    }

    pub fn versions(&self) -> impl Iterator<Item = &VersionRecord> {
        self.state.versions.values()
    }

    pub fn set_current(&mut self, relation: &str, source: &str, version: &str) {
        self.state.current.entry(relation.to_string()).or_default().insert(source.to_string(), version.to_string());
    }

    /// The current version of a relation from a source; from any source
    /// when the relation has one only.
    pub fn current(&self, relation: &str, source: Option<&str>) -> Option<&str> {
        let by_source = self.state.current.get(relation)?;
        match source {
            Some(s) => by_source.get(s).map(|v| v.as_str()),
            None if by_source.len() == 1 => by_source.values().next().map(|v| v.as_str()),
            None => None,
        }
    }

    /// Every (relation, source, current version).
    pub fn currents(&self) -> Vec<(String, String, String)> {
        let mut out = Vec::new();
        for (r, by) in &self.state.current {
            for (s, v) in by {
                out.push((r.clone(), s.clone(), v.clone()));
            }
        }
        out
    }

    // -------------------------------------------------------- cache, pins

    pub fn cache_lookup(&self, key: &str) -> Option<&(String, String)> {
        self.state.cache.get(key)
    }

    pub fn cache_record(&mut self, key: &str, relation: &str, version: &str) {
        self.state.cache.insert(key.to_string(), (relation.to_string(), version.to_string()));
    }

    pub fn pin(&mut self, version: &str, by: &str) {
        let list = self.state.pins.entry(version.to_string()).or_default();
        if !list.iter().any(|b| b == by) {
            list.push(by.to_string());
        }
    }

    pub fn unpin(&mut self, version: &str, by: &str) {
        if let Some(list) = self.state.pins.get_mut(version) {
            list.retain(|b| b != by);
            if list.is_empty() {
                self.state.pins.remove(version);
            }
        }
    }

    pub fn pinned(&self, version: &str) -> bool {
        self.state.pins.contains_key(version)
    }

    /// Forgets a version (after gc removed its files).
    pub fn drop_version(&mut self, id: &str) {
        self.state.versions.remove(id);
        self.state.cache.retain(|_, (_, v)| v != id);
    }
}

/// Definition D-2.1: exactly one event time, at most one receipt time, named
/// columns, no name twice.
pub fn validate_schema(s: &RelationSchema) -> Result<(), DataError> {
    let times = s.columns.iter().filter(|c| c.role == Role::EventTime).count();
    let receipts = s.columns.iter().filter(|c| c.role == Role::ReceiptTime).count();
    if times != 1 {
        return Err(DataError::Schema(format!("`{}` has {} event-time columns; a relation has exactly one", s.name, times)));
    }
    if receipts > 1 {
        return Err(DataError::Schema(format!("`{}` has {} receipt-time columns; a relation has at most one", s.name, receipts)));
    }
    let mut names: Vec<&str> = s.columns.iter().map(|c| c.name.as_str()).collect();
    names.sort_unstable();
    if let Some(w) = names.windows(2).find(|w| w[0] == w[1]) {
        return Err(DataError::Schema(format!("`{}` names the column `{}` twice", s.name, w[0])));
    }
    for c in &s.columns {
        if (c.role == Role::EventTime || c.role == Role::ReceiptTime) && c.ty != ColumnType::Time {
            return Err(DataError::Schema(format!("`{}`'s time column `{}` is not a time", s.name, c.name)));
        }
    }
    Ok(())
}
