//! Parquet tables (`src/table.rs`): typed columns round-trip, text cells
//! parse as the typed ones would, and messages name the file and row.

use absolute_backtest::kernel::time::parse_timestamp;
use absolute_backtest::table::{to_text, write_table, write_text_table, Cell, Col, Table};

fn scratch(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("abt-table-{}-{}", std::process::id(), name));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn typed_columns_round_trip_with_nulls() {
    let dir = scratch("typed");
    let path = dir.join("t.parquet");
    let t0 = parse_timestamp("2024-01-08").unwrap();
    let t1 = parse_timestamp("2024-01-08T09:31:00").unwrap();
    write_table(
        &path,
        vec![
            ("Name", Col::Str(vec![Some("AAA".into()), None])),
            ("n", Col::Int(vec![Some(3), Some(-1)])),
            ("x", Col::Float(vec![Some(1.5), None])),
            ("ok", Col::Bool(vec![Some(true), Some(false)])),
            ("t", Col::Time(vec![Some(t0), Some(t1)])),
        ],
    )
    .unwrap();
    let t = Table::read(&path).unwrap();
    assert_eq!(t.len(), 2);
    assert_eq!(t.names(), ["name", "n", "x", "ok", "t"], "names are lower-cased");
    assert_eq!(t.cell(0, 0), Cell::Str("AAA"));
    assert_eq!(t.text(0, 1), None);
    assert_eq!(t.integer(1, 1).unwrap(), -1);
    assert_eq!(t.number(1, 0).unwrap(), 3.0, "an integer column reads as a number");
    assert_eq!(t.number_opt(2, 1).unwrap(), None);
    assert_eq!(t.cell(3, 0), Cell::Bool(true));
    assert_eq!(t.time(4, 0).unwrap(), t0);
    assert_eq!(t.time(4, 1).unwrap(), t1, "a time of day survives the millisecond storage");
    assert_eq!(to_text(&path).unwrap(), "name,n,x,ok,t\nAAA,3,1.5,true,2024-01-08\n,-1,,false,2024-01-08T09:31:00\n");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn text_cells_parse_and_errors_name_the_row() {
    let dir = scratch("text");
    let path = dir.join("prices.parquet");
    write_text_table(&path, "symbol,date,close\nX,2024-01-08,10.5\nY,2024-01-09T16:00:00,ten\nZ,,1\n").unwrap();
    let t = Table::read(&path).unwrap();
    let (cd, cc) = (t.col("date").unwrap(), t.col("close").unwrap());
    assert_eq!(t.time(cd, 0).unwrap(), parse_timestamp("2024-01-08").unwrap());
    assert_eq!(t.number(cc, 0).unwrap(), 10.5);
    let err = t.number(cc, 1).unwrap_err();
    assert!(err.contains("prices.parquet row 2") && err.contains("ten"), "{}", err);
    assert_eq!(t.time_opt(cd, 2).unwrap(), None, "an empty field is null");
    assert!(t.time(cd, 2).unwrap_err().contains("row 3"));
    assert!(t.col("volume").unwrap_err().contains("no column `volume`"));
    let _ = std::fs::remove_dir_all(&dir);
}
