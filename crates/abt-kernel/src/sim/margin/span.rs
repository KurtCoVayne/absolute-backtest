//! `span`, model 4 of the parent module, whose documentation states the
//! rules and their source: per combined commodity, the scan risk, the
//! intra-commodity spread charge, the inter-commodity credits (in priority
//! order, each consuming the deltas it spreads) and the short option
//! minimum.

use std::collections::BTreeMap;

use super::{apportion, first_max, Context, Holding, InterCredit, Requirement, RiskArray, Source, Terms};

/// A net delta within this fraction of the gross delta is zero.
const FLAT: f64 = 1e-9;

/// One combined commodity of the book.
struct Commodity<'a> {
    legs: Vec<(&'a Holding, &'a RiskArray)>,
    scan: f64,
    /// The scenario of the scan risk.
    worst: usize,
    intra: f64,
    net_delta: f64,
    short_minimum: f64,
    credit: f64,
}

/// The rows of the instruments `span` prices, by combined commodity.
pub(super) fn rows(holdings: &[Holding], ctx: &Context) -> Vec<Requirement> {
    let mut legs: BTreeMap<&str, Vec<(&Holding, &RiskArray)>> = BTreeMap::new();
    for h in holdings {
        if let Some(a) = ctx.risk_arrays.get(&h.instrument) {
            legs.entry(a.combined.as_str()).or_default().push((h, a));
        }
    }
    let mut commodities: BTreeMap<&str, Commodity> = legs
        .into_iter()
        .map(|(cc, legs)| {
            let charge = ctx.span.intra_spread_charge.get(cc).copied().unwrap_or(0.0);
            (cc, Commodity::new(legs, charge))
        })
        .collect();
    credit(&mut commodities, &ctx.span.inter_credits);
    commodities.iter().flat_map(|(cc, c)| c.rows(ctx.span.initial_ratio.get(*cc).copied().unwrap_or(1.0))).collect()
}

impl<'a> Commodity<'a> {
    fn new(legs: Vec<(&'a Holding, &'a RiskArray)>, charge: f64) -> Commodity<'a> {
        let totals: [f64; 16] = std::array::from_fn(|k| legs.iter().map(|(h, a)| h.qty * a.scenarios[k]).sum());
        let (worst, scan) = first_max(&totals);
        let (mut long, mut short) = (0.0, 0.0);
        for (h, a) in &legs {
            let d = h.qty * a.delta;
            if d > 0.0 {
                long += d;
            } else {
                short -= d;
            }
        }
        let net = long - short;
        let net_delta = if net.abs() <= FLAT * (long + short) { 0.0 } else { net };
        let short_minimum = legs
            .iter()
            .filter(|(h, _)| h.qty < 0.0 && matches!(h.terms, Terms::Option { .. }))
            .map(|(h, a)| -h.qty * a.short_option_min)
            .sum();
        Commodity {
            scan: scan.max(0.0),
            worst,
            intra: long.min(short) * charge,
            net_delta,
            short_minimum,
            credit: 0.0,
            legs,
        }
    }

    /// The weighted futures price risk: the scan risk per net delta.
    fn price_risk(&self) -> f64 {
        if self.net_delta == 0.0 {
            0.0
        } else {
            self.scan / self.net_delta.abs()
        }
    }

    /// The commodity's rows: maintenance max(scan + intra − credit, the
    /// short option minimum, 0), by the losses at the worst scenario.
    fn rows(&self, ratio: f64) -> Vec<Requirement> {
        let maintenance = (self.scan + self.intra - self.credit).max(self.short_minimum).max(0.0);
        let losses: Vec<f64> = self.legs.iter().map(|(h, a)| (h.qty * a.scenarios[self.worst]).max(0.0)).collect();
        let weights = if losses.iter().sum::<f64>() > 0.0 {
            losses
        } else {
            self.legs.iter().map(|(h, _)| h.qty.abs()).collect()
        };
        let parts = apportion(maintenance, &weights);
        self.legs
            .iter()
            .zip(parts)
            .map(|((h, _), m)| Requirement {
                instrument: h.instrument,
                initial: m * ratio,
                maintenance: m,
                source: Source::Span,
            })
            .collect()
    }
}

/// The inter-commodity credits, in the order listed: a pair whose remaining
/// net deltas have opposite signs forms spreads, credited to each side by
/// its own price risk, and the deltas spread are consumed.
fn credit(commodities: &mut BTreeMap<&str, Commodity>, credits: &[InterCredit]) {
    let risk: BTreeMap<&str, f64> = commodities.iter().map(|(cc, c)| (*cc, c.price_risk())).collect();
    let mut left: BTreeMap<&str, f64> = commodities.iter().map(|(cc, c)| (*cc, c.net_delta)).collect();
    for ic in credits {
        let (Some(&da), Some(&db)) = (left.get(ic.a.as_str()), left.get(ic.b.as_str())) else {
            continue;
        };
        let spreadable = ic.a != ic.b && da * db < 0.0 && ic.ratio_a > 0.0 && ic.ratio_b > 0.0;
        if !spreadable {
            continue;
        }
        let spreads = (da.abs() / ic.ratio_a).min(db.abs() / ic.ratio_b);
        for (cc, delta, ratio) in [(ic.a.as_str(), da, ic.ratio_a), (ic.b.as_str(), db, ic.ratio_b)] {
            if let Some(c) = commodities.get_mut(cc) {
                c.credit += ic.rate * spreads * risk.get(cc).copied().unwrap_or(0.0) * ratio;
            }
            if let Some(d) = left.get_mut(cc) {
                *d = delta - delta.signum() * spreads * ratio;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::SpanParams;
    use super::*;

    fn future(id: u32, qty: f64) -> Holding {
        Holding {
            instrument: id,
            qty,
            mark: 0.0,
            terms: Terms::Future { point_value: 50.0 },
        }
    }

    fn array(combined: &str, scale: f64) -> RiskArray {
        let mut scenarios = [0.0; 16];
        for (k, s) in scenarios.iter_mut().enumerate() {
            let size = (k / 2 + 1) as f64;
            *s = if k % 2 == 0 { scale * size } else { -scale * size };
        }
        RiskArray {
            combined: combined.to_string(),
            scenarios,
            delta: 1.0,
            short_option_min: 0.0,
        }
    }

    #[test]
    fn a_delta_earns_one_credit() {
        // Arrays ±scale × 1..8: A long 4 at scale 100 (scan 4 × 800 = 3,200,
        // ND +4, pr 800); B short 2 at 100 (scan 1,600, ND −2, pr 800); C
        // short 4 at 100 (scan 3,200, ND −4, pr 800). The first credit (A:B,
        // 2:1 at 50 %) forms min(4/2, 2/1) = 2 spreads: A gets 0.5 × 2 × 800
        // × 2 = 1,600, B 0.5 × 2 × 800 × 1 = 800, and A's 4 deltas and B's 2
        // are consumed, so the second (A:C, 1:1) finds none of A's left.
        let holdings = [future(1, 4.0), future(2, -2.0), future(3, -4.0)];
        let arrays: BTreeMap<u32, RiskArray> = [(1, array("A", 100.0)), (2, array("B", 100.0)), (3, array("C", 100.0))].into();
        let credits = vec![
            InterCredit {
                a: "A".into(),
                b: "B".into(),
                ratio_a: 2.0,
                ratio_b: 1.0,
                rate: 0.5,
            },
            InterCredit {
                a: "A".into(),
                b: "C".into(),
                ratio_a: 1.0,
                ratio_b: 1.0,
                rate: 0.5,
            },
        ];
        let span = SpanParams {
            inter_credits: credits,
            ..SpanParams::default()
        };
        let (underlyings, published) = (BTreeMap::new(), BTreeMap::new());
        let ctx = Context {
            t: 0,
            underlyings: &underlyings,
            published: &published,
            risk_arrays: &arrays,
            span: &span,
            reprice: &|_, _| None,
        };
        let rows = rows(&holdings, &ctx);
        let maintenance: Vec<f64> = rows.iter().map(|r| r.maintenance).collect();
        // A 3,200 − 1,600 = 1,600; B 1,600 − 800 = 800; C 3,200.
        assert_eq!(maintenance, [1600.0, 800.0, 3200.0]);
    }
}
