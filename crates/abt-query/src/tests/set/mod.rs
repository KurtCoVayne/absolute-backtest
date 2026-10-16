//! The starting set (Q-4.4): every test registered at start, each in its
//! own module; none is privileged over a developer's (Q-4.5).

use std::sync::Arc;

use super::framework::Test;

pub mod regression;

/// The starting set, in name order.
pub fn starting_set() -> Vec<Arc<dyn Test>> {
    vec![Arc::new(regression::Regression)]
}
