//! The starting set (Q-4.4): every test registered at start, each in its
//! own module; none is privileged over a developer's (Q-4.5).

use std::sync::Arc;

use super::framework::Test;

pub mod adf;
pub mod arch_lm;
pub mod coactivation;
pub mod cost_realism;
pub mod effective_rules;
pub mod engle_granger;
pub mod fama_macbeth;
pub mod half_life;
pub mod ic;
pub mod johansen;
pub mod kpss;
pub mod ljung_box;
pub mod regression;
pub mod timeseries;
pub mod variance_ratio;

/// The starting set, in name order.
pub fn starting_set() -> Vec<Arc<dyn Test>> {
    vec![
        Arc::new(adf::Adf),
        Arc::new(arch_lm::ArchLm),
        Arc::new(coactivation::Coactivation),
        Arc::new(cost_realism::CostRealism),
        Arc::new(effective_rules::EffectiveRules),
        Arc::new(engle_granger::EngleGranger),
        Arc::new(fama_macbeth::FamaMacBeth),
        Arc::new(half_life::HalfLife),
        Arc::new(ic::Ic),
        Arc::new(johansen::Johansen),
        Arc::new(kpss::Kpss),
        Arc::new(ljung_box::LjungBox),
        Arc::new(regression::Regression),
        Arc::new(variance_ratio::VarianceRatio),
    ]
}
