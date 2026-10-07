//! The stored relations' index (src/kernel/mod.rs `Store`): an entity-bound
//! lookup is the scan's answer, through inserts in any order, re-sorts and
//! unindexed stores.

use absolute_backtest::data::Rng;
use absolute_backtest::kernel::{Store, Value};

fn scan(st: &Store, key: i64, e: usize, sym: u32) -> Vec<Vec<Value>> {
    let mut v: Vec<Vec<Value>> = st.by_time.get(&key).map(|b| b.iter().filter(|t| t[e] == Value::Equity(sym)).cloned().collect()).unwrap_or_default();
    v.sort();
    v
}

#[test]
fn an_indexed_lookup_equals_the_scan() {
    let mut rng = Rng::new(7);
    let mut st = Store::default();
    st.index_by(Some(0));
    for round in 0..4 {
        for _ in 0..2_000 {
            let key = (rng.next_u64() % 5) as i64;
            let sym = (rng.next_u64() % 50) as u32;
            st.insert(key, vec![Value::Equity(sym), Value::Time(key), Value::Num(rng.uniform())]);
        }
        for key in 0..6 {
            for sym in 0..55 {
                let reference = scan(&st, key, 0, sym);
                let mut got: Vec<Vec<Value>> = st.lookup(key, Some(&Value::Equity(sym))).to_vec();
                got.sort();
                assert_eq!(got, reference, "round {} key {} sym {}", round, key, sym);
            }
            let all = st.by_time.get(&key).map(|b| b.len()).unwrap_or(0);
            assert_eq!(st.lookup(key, None).len(), all);
        }
    }
    // Unindexed: every lookup is the whole block.
    let mut plain = Store::default();
    plain.insert(1, vec![Value::Time(1), Value::Num(1.0)]);
    plain.insert(1, vec![Value::Time(1), Value::Num(2.0)]);
    assert_eq!(plain.lookup(1, Some(&Value::Equity(3))).len(), 2);
}
