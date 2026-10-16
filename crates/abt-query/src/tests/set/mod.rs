//! The starting set (Q-4.4): every test registered at start, each in its
//! own module; none is privileged over a developer's (Q-4.5).

use std::sync::Arc;

use super::framework::Test;

pub mod adf;
pub mod arch_lm;
pub mod bootstrap_sharpe;
pub mod coactivation;
pub mod cost_realism;
pub mod dsr;
pub mod effective_rules;
pub mod engle_granger;
pub mod fama_macbeth;
pub mod half_life;
pub mod hill_tail;
pub mod ic;
pub mod jarque_bera;
pub mod johansen;
pub mod kpss;
pub mod ks_normal;
pub mod ljung_box;
pub mod min_trl;
pub mod nw_mean;
pub mod pbo;
pub mod psr;
pub mod regression;
pub mod samples;
pub mod timeseries;
pub mod variance_ratio;
pub mod wfe;

/// The starting set, in name order.
pub fn starting_set() -> Vec<Arc<dyn Test>> {
    vec![
        Arc::new(adf::Adf),
        Arc::new(arch_lm::ArchLm),
        Arc::new(bootstrap_sharpe::BootstrapSharpe),
        Arc::new(coactivation::Coactivation),
        Arc::new(cost_realism::CostRealism),
        Arc::new(dsr::Dsr),
        Arc::new(effective_rules::EffectiveRules),
        Arc::new(engle_granger::EngleGranger),
        Arc::new(fama_macbeth::FamaMacBeth),
        Arc::new(half_life::HalfLife),
        Arc::new(hill_tail::HillTail),
        Arc::new(ic::Ic),
        Arc::new(jarque_bera::JarqueBera),
        Arc::new(johansen::Johansen),
        Arc::new(kpss::Kpss),
        Arc::new(ks_normal::KsNormal),
        Arc::new(ljung_box::LjungBox),
        Arc::new(min_trl::MinTrl),
        Arc::new(nw_mean::NwMean),
        Arc::new(pbo::Pbo),
        Arc::new(psr::Psr),
        Arc::new(regression::Regression),
        Arc::new(variance_ratio::VarianceRatio),
        Arc::new(wfe::Wfe),
    ]
}
