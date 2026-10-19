//! A table the query layer computes (a feed or activation statistic, a
//! report's table, Q-6 to Q-8): named columns and rows of cells, rendered as
//! text and JSON and written as a relation of the data layer.

use abt_wire::generated::abt::wire::{RelationSchema, Role};
use arrow::record_batch::RecordBatch;

use crate::data::Error;
use crate::relation::{Col, Rows};

/// A cell.
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
#[serde(untagged)]
pub enum Cell {
    Null,
    Text(String),
    Num(f64),
    Int(i64),
    Time(i64),
    Bool(bool),
}

impl Cell {
    pub fn num(x: f64) -> Cell {
        if x.is_finite() {
            Cell::Num(x)
        } else {
            Cell::Null
        }
    }

    pub fn text(s: impl Into<String>) -> Cell {
        Cell::Text(s.into())
    }

    fn render(&self) -> String {
        match self {
            Cell::Null => "-".into(),
            Cell::Text(s) => s.clone(),
            Cell::Num(x) => {
                if x.fract() == 0.0 && x.abs() < 1e15 {
                    format!("{:.0}", x)
                } else {
                    format!("{:.6}", x)
                }
            }
            Cell::Int(i) => i.to_string(),
            Cell::Time(t) => crate::time::format_timestamp(*t),
            Cell::Bool(b) => b.to_string(),
        }
    }
}

/// A table of cells.
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct Sheet {
    pub name: String,
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Cell>>,
}

impl Sheet {
    pub fn new(name: &str, columns: &[&str]) -> Sheet {
        Sheet {
            name: name.to_string(),
            columns: columns.iter().map(|c| c.to_string()).collect(),
            rows: vec![],
        }
    }

    pub fn push(&mut self, row: Vec<Cell>) {
        debug_assert_eq!(row.len(), self.columns.len(), "{}: a row of {} cells for {} columns", self.name, row.len(), self.columns.len());
        self.rows.push(row);
    }

    /// The column named `name`'s cells.
    pub fn column(&self, name: &str) -> Option<Vec<&Cell>> {
        let i = self.columns.iter().position(|c| c == name)?;
        Some(self.rows.iter().map(|r| &r[i]).collect())
    }

    /// The table as aligned text, its name first.
    pub fn text(&self) -> String {
        let cells: Vec<Vec<String>> = self.rows.iter().map(|r| r.iter().map(Cell::render).collect()).collect();
        let widths: Vec<usize> = (0..self.columns.len())
            .map(|j| cells.iter().map(|r| r[j].len()).chain([self.columns[j].len()]).max().unwrap_or(0))
            .collect();
        let line = |r: &[String]| r.iter().zip(&widths).map(|(c, w)| format!("{:<w$}", c, w = *w)).collect::<Vec<_>>().join("  ").trim_end().to_string();
        let mut out = format!("{}\n", self.name);
        out.push_str(&format!("  {}\n", line(&self.columns)));
        for r in &cells {
            out.push_str(&format!("  {}\n", line(r)));
        }
        out
    }

    /// The table as a relation: a row per row, keyed by its position and
    /// the time `at`; a column of one kind of cell keeps its type, a mixed
    /// one is text.
    pub fn relation(&self, name: &str, at: i64) -> Result<(RelationSchema, RecordBatch), Error> {
        let n = self.rows.len();
        let mut rows = Rows::new(name)
            .col("row", Role::Entity, Col::Entity((0..n as u64).map(Some).collect()))
            .col("t", Role::EventTime, Col::Time(vec![Some(at); n]));
        for (j, c) in self.columns.iter().enumerate() {
            let cells: Vec<&Cell> = self.rows.iter().map(|r| &r[j]).collect();
            let kinds: std::collections::BTreeSet<u8> = cells
                .iter()
                .filter_map(|c| match c {
                    Cell::Null => None,
                    Cell::Text(_) => Some(0),
                    Cell::Num(_) => Some(1),
                    Cell::Int(_) => Some(2),
                    Cell::Time(_) => Some(3),
                    Cell::Bool(_) => Some(4),
                })
                .collect();
            let col = match kinds.iter().copied().collect::<Vec<_>>().as_slice() {
                [1] | [1, 2] => Col::Num(
                    cells
                        .iter()
                        .map(|c| match c {
                            Cell::Num(x) => Some(*x),
                            Cell::Int(i) => Some(*i as f64),
                            _ => None,
                        })
                        .collect(),
                ),
                [2] => Col::Count(cells.iter().map(|c| if let Cell::Int(i) = c { Some(*i) } else { None }).collect()),
                [3] => Col::Time(cells.iter().map(|c| if let Cell::Time(t) = c { Some(*t) } else { None }).collect()),
                [4] => Col::Bool(cells.iter().map(|c| if let Cell::Bool(b) = c { Some(*b) } else { None }).collect()),
                _ => Col::Bytes(cells.iter().map(|c| if **c == Cell::Null { None } else { Some(c.render()) }).collect()),
            };
            // `row` and `t` are the key.
            let name = if c == "row" || c == "t" { format!("{}_", c) } else { c.clone() };
            rows = rows.col(&name, Role::Value, col);
        }
        rows.finish()
    }
}
