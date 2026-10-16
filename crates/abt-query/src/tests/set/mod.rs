//! The starting set (Q-4.4): every test registered at start, each in its
//! own module; none is privileged over a developer's (Q-4.5).

use std::sync::Arc;

use super::framework::Test;

pub mod coactivation;
pub mod cost_realism;
pub mod effective_rules;
pub mod fama_macbeth;
pub mod ic;
pub mod regression;

/// The starting set, in name order.
pub fn starting_set() -> Vec<Arc<dyn Test>> {
    vec![
        Arc::new(coactivation::Coactivation),
        Arc::new(cost_realism::CostRealism),
        Arc::new(effective_rules::EffectiveRules),
        Arc::new(fama_macbeth::FamaMacBeth),
        Arc::new(ic::Ic),
        Arc::new(regression::Regression),
    ]
}
