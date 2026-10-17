//! Dynamic needs (D-7.6): the chains a program queries per barrier. The
//! pre-pass that evaluates their filters over the span and resolves the
//! contracts they admit as static needs reads the option class's
//! relations, which land at M-7; until then a demand with a dynamic need
//! is refused, naming it, and so is `resolve_dynamic`.

use abt_wire::generated::abt::wire::{Demand, DynamicNeed};

use crate::error::DataError;

fn refusal(d: &DynamicNeed) -> DataError {
    DataError::Unresolved(format!(
        "the demand asks for `{}` chains, and no source offers option chains yet (the option class and its adapters land at M-7)",
        d.relation
    ))
}

/// A demand's dynamic needs, resolved (none yet can be).
pub fn check(demand: &Demand) -> Result<(), DataError> {
    match demand.dynamic.as_ref().and_then(|d| d.first()) {
        Some(d) => Err(refusal(d)),
        None => Ok(()),
    }
}

/// `resolve_dynamic` (P-3.3): a chain query the pre-pass did not cover.
pub fn resolve_dynamic(need: &DynamicNeed) -> Result<(), DataError> {
    Err(refusal(need))
}
