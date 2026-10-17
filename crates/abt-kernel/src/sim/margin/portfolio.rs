//! `portfolio`, model 3 of the parent module, whose documentation states the
//! rules and their sources: the shock grid, a holding's values over it, and
//! the requirement of one underlying's group with its attribution.

use std::collections::BTreeMap;

use super::{apportion, first_max, priced, underlying_of, Context, Holding, IndexClass, Requirement, Source, Terms};

/// The requirement's floor per option contract held, times the multiplier
/// (FINRA 4210(g)).
const PER_CONTRACT: f64 = 0.375;

/// The shock range of an underlying's class, as K-5.6 states it.
pub fn range(class: IndexClass) -> (f64, f64) {
    match class {
        IndexClass::Equity => (-0.15, 0.15),
        IndexClass::BroadIndex => (-0.08, 0.08),
        IndexClass::NarrowIndex => (-0.06, 0.08),
    }
}

/// The grid: eleven equally spaced shocks over the class's range, ends
/// included.
pub fn grid(class: IndexClass) -> [f64; 11] {
    let (lo, hi) = range(class);
    std::array::from_fn(|i| (lo * (10 - i) as f64 + hi * i as f64) / 10.0)
}

/// A holding valued now and at each point of its underlying's grid, per
/// unit, with its weight (m for an option, 1 for stock).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Leg {
    holding: Holding,
    weight: f64,
    option: bool,
    now: f64,
    shocked: [f64; 11],
}

impl Leg {
    /// The leg's loss at grid point `k` (a gain is negative).
    fn loss(&self, k: usize) -> f64 {
        -self.holding.qty * (self.shocked[k] - self.now) * self.weight
    }
}

/// The leg of a holding the model can value, or why it cannot: a share is
/// worth its underlying's price (its mark when it has no entry, as an
/// equity), an option what `reprice` gives.
pub(super) fn leg(h: &Holding, ctx: &Context) -> Result<Leg, String> {
    match h.terms {
        Terms::Future { .. } => Err("a future has no portfolio rule".to_string()),
        Terms::Stock => {
            let (s, class) = priced(ctx, h.instrument).map_or((h.mark, IndexClass::Equity), |u| (u.price, u.class));
            Ok(Leg {
                holding: *h,
                weight: 1.0,
                option: false,
                now: s,
                shocked: grid(class).map(|x| s * (1.0 + x)),
            })
        }
        Terms::Option { underlying, multiplier, .. } => {
            let u = priced(ctx, underlying)?;
            let value = |s: f64| {
                (ctx.reprice)(h, s)
                    .filter(|v| v.is_finite())
                    .ok_or_else(|| "the option cannot be repriced (no implied volatility)".to_string())
            };
            let now = value(u.price)?;
            let mut shocked = [0.0; 11];
            for (v, x) in shocked.iter_mut().zip(grid(u.class)) {
                *v = value(u.price * (1.0 + x))?;
            }
            Ok(Leg {
                holding: *h,
                weight: multiplier,
                option: true,
                now,
                shocked,
            })
        }
    }
}

/// The rows of the instruments `portfolio` prices, grouped by underlying.
pub(super) fn rows(legs: &[Leg]) -> Vec<Requirement> {
    let mut groups: BTreeMap<u32, Vec<&Leg>> = BTreeMap::new();
    for leg in legs {
        groups.entry(underlying_of(&leg.holding)).or_default().push(leg);
    }
    groups.values().flat_map(|group| rows_of_group(group)).collect()
}

/// One underlying's requirement, the worst loss over the grid floored at
/// $0.375 × m a contract, attributed by the losses at the worst point.
fn rows_of_group(legs: &[&Leg]) -> Vec<Requirement> {
    let totals: [f64; 11] = std::array::from_fn(|k| legs.iter().map(|l| l.loss(k)).sum());
    let (worst, loss) = first_max(&totals);
    let loss = loss.max(0.0);
    let floor: f64 = legs.iter().filter(|l| l.option).map(|l| PER_CONTRACT * l.weight * l.holding.qty.abs()).sum();
    let losses: Vec<f64> = legs.iter().map(|l| l.loss(worst).max(0.0)).collect();
    let weights = if floor > loss || losses.iter().sum::<f64>() <= 0.0 {
        legs.iter().map(|l| (l.holding.qty * l.weight).abs()).collect()
    } else {
        losses
    };
    let parts = apportion(loss.max(floor), &weights);
    legs.iter()
        .zip(parts)
        .map(|(l, r)| Requirement {
            instrument: l.holding.instrument,
            initial: r,
            maintenance: r,
            source: Source::Portfolio,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_grids() {
        // −15 %..+15 % in steps of 3 %, −8 %..+8 % in steps of 1.6 %, −6 %..+8 %
        // in steps of 1.4 %.
        let close = |a: [f64; 11], b: [f64; 11]| a.iter().zip(&b).all(|(x, y)| (x - y).abs() < 1e-15);
        assert!(close(grid(IndexClass::Equity), [-0.15, -0.12, -0.09, -0.06, -0.03, 0.0, 0.03, 0.06, 0.09, 0.12, 0.15]));
        assert!(close(grid(IndexClass::BroadIndex), [-0.08, -0.064, -0.048, -0.032, -0.016, 0.0, 0.016, 0.032, 0.048, 0.064, 0.08]));
        assert!(close(grid(IndexClass::NarrowIndex), [-0.06, -0.046, -0.032, -0.018, -0.004, 0.01, 0.024, 0.038, 0.052, 0.066, 0.08]));
        assert_eq!(grid(IndexClass::Equity)[5], 0.0);
    }
}
