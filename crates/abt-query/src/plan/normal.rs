//! A plan's normal form and its hash H(Π) (Q-2.4, A-6): the plan's blocks
//! with the template substituted (which `parse::plan` did), the names of
//! its steps and claims canonicalised by first occurrence, the steps in
//! topological order with the canonical tie-break (the step's kind, then
//! its inputs), the `report` block and steps removed, the program named by
//! its hash H(P). H(Π) is the BLAKE3 of the normal form's text; two plans
//! with one normal form are one plan, whatever they are called.

use std::collections::BTreeMap;

use super::{render_point, Plan, Step, KINDS};

/// A plan's normal form.
#[derive(Clone, Debug, PartialEq)]
pub struct Normal {
    /// The text H(Π) is over.
    pub text: String,
    /// H(Π), 64 hex digits.
    pub hash: String,
    /// The steps in the order they execute, each by its index in the plan
    /// with its canonical name; the report steps last.
    pub order: Vec<(usize, String)>,
    /// The canonical name of every step and claim, by its written name.
    pub names: BTreeMap<String, String>,
}

impl Normal {
    /// The canonical name of a step or claim written `name`.
    pub fn canonical<'a>(&'a self, name: &'a str) -> &'a str {
        self.names.get(name).map(String::as_str).unwrap_or(name)
    }
}

/// The normal form of `plan`, whose program's hash is `program_hash`.
pub fn normal_form(plan: &Plan, program_hash: &str) -> Result<Normal, String> {
    let n = plan.steps.len();
    let mut names: BTreeMap<String, String> = BTreeMap::new();
    let mut index: BTreeMap<String, usize> = BTreeMap::new();
    let mut order: Vec<(usize, String)> = Vec::new();
    let mut done: Vec<bool> = plan.steps.iter().map(|s| matches!(s, Step::Report { .. })).collect();
    loop {
        let ready: Vec<usize> = (0..n).filter(|&i| !done[i] && plan.steps[i].inputs().iter().all(|x| index.contains_key(x))).collect();
        // The tie-break: the kind, then the positions of the inputs, then
        // the step as written with its inputs' canonical names (two steps
        // equal in all three are one step twice, in either order).
        let Some(next) = ready.into_iter().min_by_key(|&i| {
            let s = &plan.steps[i];
            let kind = KINDS.iter().position(|k| *k == s.kind()).unwrap_or(KINDS.len());
            let inputs: Vec<usize> = s.inputs().iter().map(|x| index[x]).collect();
            let text = s.render("_", &|x: &str| names.get(x).cloned().unwrap_or_else(|| x.to_string()));
            (kind, inputs, text)
        }) else {
            break;
        };
        let canonical = format!("s{}", order.len());
        names.insert(plan.steps[next].name().to_string(), canonical.clone());
        index.insert(plan.steps[next].name().to_string(), order.len());
        order.push((next, canonical));
        done[next] = true;
    }
    if let Some(i) = (0..n).find(|&i| !done[i]) {
        return Err(format!("study {}: step `{}` is on a cycle of steps that read each other", plan.name, plan.steps[i].name()));
    }
    let step_name = |x: &str| names.get(x).cloned().unwrap_or_else(|| x.to_string());
    // The claims by their text with the steps' canonical names, named in
    // that order.
    let mut claims: Vec<(String, usize)> = plan.claims.iter().enumerate().map(|(i, c)| (c.render("_", &step_name), i)).collect();
    claims.sort();
    let mut text = String::from("study {\n");
    text.push_str(&format!("  program {}\n", program_hash));
    if !plan.params.is_empty() {
        text.push_str(&format!("  params {}\n", render_point(&plan.params)));
    }
    if !plan.grid.is_empty() {
        let axes: Vec<String> = plan.grid.iter().map(|(k, vs)| format!("{} = {}", k, vs.join(", "))).collect();
        text.push_str(&format!("  grid {{ {} }}\n", axes.join("; ")));
    }
    text.push_str(&format!("  holdout {}\n", plan.holdout.describe()));
    if let Some(wf) = &plan.walk_forward {
        text.push_str(&format!("  walk_forward {}\n", wf.describe()));
    }
    if let Some(w) = &plan.window {
        text.push_str(&format!("  window {}\n", w.describe()));
    }
    if let Some(b) = &plan.budget {
        let fields: Vec<String> = [("money", &b.money), ("bytes", &b.bytes), ("requests", &b.requests)]
            .into_iter()
            .filter_map(|(k, v)| v.as_ref().map(|v| format!("{} {}", k, v)))
            .collect();
        text.push_str(&format!("  budget {{ {} }}\n", fields.join("; ")));
    }
    text.push_str(&format!("  objective {}\n", plan.objective));
    let mut claim_names = Vec::new();
    if !claims.is_empty() {
        text.push_str("  claims {\n");
        for (k, (_, i)) in claims.iter().enumerate() {
            let own = format!("c{}", k);
            text.push_str(&format!("    {}\n", plan.claims[*i].render(&own, &step_name)));
            claim_names.push((plan.claims[*i].name.clone(), own));
        }
        text.push_str("  }\n");
    }
    text.push_str("  steps {\n");
    for (i, own) in &order {
        text.push_str(&format!("    {}\n", plan.steps[*i].render(own, &step_name)));
    }
    text.push_str("  }\n}\n");
    names.extend(claim_names);
    // The report steps, last, keep their written names (they are not part
    // of the normal form).
    for (i, s) in plan.steps.iter().enumerate() {
        if matches!(s, Step::Report { .. }) {
            order.push((i, s.name().to_string()));
            names.insert(s.name().to_string(), s.name().to_string());
        }
    }
    let hash = abt_wire::Hash::of(text.as_bytes()).hex();
    Ok(Normal { text, hash, order, names })
}
