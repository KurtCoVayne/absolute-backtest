//! The registry of tests (Q-4.1, Q-4.5): names to implementations. The
//! starting set (Q-4.4) registers at start; a developer adds a test by
//! implementing `Test` and calling `register`, and no test is privileged.

use std::collections::BTreeMap;
use std::sync::{Arc, OnceLock, RwLock};

use super::framework::Test;

fn registry() -> &'static RwLock<BTreeMap<String, Arc<dyn Test>>> {
    static R: OnceLock<RwLock<BTreeMap<String, Arc<dyn Test>>>> = OnceLock::new();
    R.get_or_init(|| {
        let mut m: BTreeMap<String, Arc<dyn Test>> = BTreeMap::new();
        for t in super::set::starting_set() {
            m.insert(t.name().to_string(), t);
        }
        RwLock::new(m)
    })
}

/// Registers a test under its name; a name already registered is refused,
/// since a plan names a test and its version together.
pub fn register(test: Arc<dyn Test>) -> Result<(), String> {
    let mut r = registry().write().expect("the registry");
    let name = test.name().to_string();
    if name.is_empty() || name.chars().any(|c| !(c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')) {
        return Err(format!("`{}` is not a test name: lowercase letters, digits and `_`", name));
    }
    if r.contains_key(&name) {
        return Err(format!("a test named `{}` is registered", name));
    }
    r.insert(name, test);
    Ok(())
}

/// The test registered under `name`.
pub fn get(name: &str) -> Option<Arc<dyn Test>> {
    registry().read().expect("the registry").get(name).cloned()
}

/// Every registered test's name, in order.
pub fn names() -> Vec<String> {
    registry().read().expect("the registry").keys().cloned().collect()
}
