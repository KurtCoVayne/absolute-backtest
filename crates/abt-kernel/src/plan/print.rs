//! The plan as text and JSON (compiler design, section 5.6).

use super::Plan;

impl Plan {
    pub fn text(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!(
            "plan {} (numerics {}, statistics: {}): {} rules, {} reordered, {} infeasible, {} elided, {} greedy; estimated cost per bar {:.1} (written order {:.1})\n",
            self.label,
            self.numerics.name(),
            self.statistics,
            self.totals.rules,
            self.totals.reordered,
            self.totals.infeasible,
            self.totals.elided,
            self.totals.greedy,
            self.totals.est_cost,
            self.totals.written_cost
        ));
        let observed = self.rules.iter().any(|r| r.literals.iter().any(|l| l.observed.is_some()));
        for rp in &self.rules {
            let flags = [rp.infeasible.then_some("infeasible: no work"), rp.elided.then_some("elided")]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>();
            out.push_str(&format!(
                "\n{} [{} search, {} orders; cost {:.2}, written {:.2}{}]\n",
                rp.label,
                rp.search.name(),
                rp.orders_considered,
                rp.est_cost,
                rp.written_cost,
                if flags.is_empty() { String::new() } else { format!("; {}", flags.join(", ")) }
            ));
            if observed {
                out.push_str("  pos  lit  kind         est in    est out  cost   sel     obs in   obs out  emptied  ratio\n");
            } else {
                out.push_str("  pos  lit  kind         est in    est out  cost   sel\n");
            }
            for (pos, lp) in rp.literals.iter().enumerate() {
                let mut line = format!(
                    "  {:>3}  {:>3}  {:<11} {:>8.2} {:>9.2} {:>5.1} {:>5.2}",
                    pos + 1,
                    lp.index + 1,
                    lp.kind,
                    lp.card_in,
                    lp.card_out,
                    lp.cost,
                    lp.selectivity
                );
                if observed {
                    match lp.observed {
                        Some((i, o, e)) => {
                            let ratio = if lp.card_out > 0.0 && i > 0 {
                                (o as f64 / i as f64) / (lp.card_out / lp.card_in.max(1e-9))
                            } else {
                                f64::NAN
                            };
                            let mark = if ratio.is_finite() && !(0.25..=4.0).contains(&ratio) { " revisit" } else { "" };
                            line.push_str(&format!(" {:>8} {:>9} {:>8}  {:>5.2}{}", i, o, e, ratio, mark));
                        }
                        None => line.push_str("        -         -        -      -"),
                    }
                }
                line.push_str(&format!("  {}\n", lp.text));
                out.push_str(&line);
            }
        }
        if !self.aggregates.is_empty() {
            out.push_str("\naggregates:\n");
            for a in &self.aggregates {
                out.push_str(&format!(
                    "  rule {} literal {}: {} ({}, {}): {}{}\n",
                    a.rule,
                    a.literal + 1,
                    a.agg,
                    a.class,
                    a.window,
                    a.implementation,
                    a.shares_state_of.map(|p| format!("; shares the window state of literal {}", p + 1)).unwrap_or_default()
                ));
            }
        }
        out.push_str("\nrelations:\n");
        for r in &self.relations {
            let mut tags = vec![r.kind.clone(), r.store.clone(), r.schedule.clone()];
            if r.asof_index {
                tags.push("as-of index".into());
            }
            if r.rows_window {
                tags.push("per-entity row window".into());
            }
            if let Some(h) = &r.memo_horizon {
                tags.push(format!("memo horizon {}", h));
            }
            if r.elided {
                tags.push("elided".into());
            } else if !r.demanded {
                tags.push("not demanded".into());
            }
            out.push_str(&format!("  {}  [{}]\n", r.name, tags.join(", ")));
        }
        out.push_str("\nstrata:\n");
        for s in &self.strata {
            out.push_str(&format!(
                "  {}: {} [{}{}]\n",
                s.index,
                s.relations.join(", "),
                s.schedule,
                if s.barrier { ", cross-sectional barrier" } else { "" }
            ));
        }
        for n in &self.notes {
            out.push_str(&format!("note: {}\n", n));
        }
        out
    }

    pub fn json(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or(serde_json::Value::Null)
    }
}
