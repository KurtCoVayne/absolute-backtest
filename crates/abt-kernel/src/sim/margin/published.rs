//! `published`, model 1 of the parent module (K-5.6): the requirement the
//! binding's `margin` relation states per contract (or per share), times
//! |qty|.

use super::{Holding, PublishedRow, Requirement, Source};

/// The rows of the instruments `published` prices.
pub(super) fn rows(chosen: &[(Holding, PublishedRow)]) -> Vec<Requirement> {
    chosen
        .iter()
        .map(|(h, row)| {
            let q = h.qty.abs();
            Requirement {
                instrument: h.instrument,
                initial: q * row.initial,
                maintenance: q * row.maintenance,
                source: Source::Published,
            }
        })
        .collect()
}
