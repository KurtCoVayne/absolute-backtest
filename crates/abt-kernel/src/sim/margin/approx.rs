//! `approx F`, model 5 of the parent module (K-5.6): F × the notional, initial
//! = maintenance. The notional is |qty| × |price| × point value for a future,
//! |qty| × m × S for an option (the underlying's; the strike stands in for S
//! when the context has no price) and |qty × mark| for stock.

use super::{priced, Context, Holding, Requirement, Source, Terms};

/// The rows of the instruments `approx` prices, each with its fraction.
pub(super) fn rows(chosen: &[(Holding, f64)], ctx: &Context) -> Vec<Requirement> {
    chosen
        .iter()
        .map(|(h, f)| {
            let r = f * notional(h, ctx);
            Requirement {
                instrument: h.instrument,
                initial: r,
                maintenance: r,
                source: Source::Approx,
            }
        })
        .collect()
}

/// The notional `approx` takes its fraction of.
fn notional(h: &Holding, ctx: &Context) -> f64 {
    let q = h.qty.abs();
    match h.terms {
        Terms::Stock => q * h.mark.abs(),
        Terms::Future { point_value } => q * h.mark.abs() * point_value,
        Terms::Option { underlying, strike, multiplier, .. } => q * multiplier * priced(ctx, underlying).map_or(strike, |u| u.price).abs(),
    }
}
