//! Observability (`docs/language-v2.md`, section 8, as far as the v1 kernel
//! carries it): the program graph a reader or a model sees before a run, and
//! the coverage report after one, which names the first relation that was
//! demanded and never held on every path to `decide`.
//!
//! Both read the checked program only (`Program`: signatures, rules, the
//! per-rule references the checker recorded, strata, completeness); the
//! coverage report adds the kernel's per-relation counts (`RelationCoverage`).

use std::collections::{BTreeSet, HashMap, VecDeque};

use crate::check::{tarjan, Polarity, Program};
use crate::ir::{Kind, Mode};
use crate::kernel::RelationCoverage;

/// One relation of the program graph.
#[derive(Clone, Debug)]
pub struct Node {
    pub name: String,
    /// `primitive`, `executor`, `kernel`, `derived` or `output`.
    pub kind: &'static str,
    pub signature: String,
    pub resolution: String,
    pub complete: bool,
    /// Index of the stratum the relation is evaluated in (derived relations).
    pub stratum: Option<usize>,
    /// The relation depends on itself through earlier bars (WF-4).
    pub recursive: bool,
    /// The relation reads, transitively, what the executor produced
    /// (`position`, `cash`, `nav`, `fill`, `decided`): it is evaluated bar
    /// by bar, after the previous bar's fills (F-11.5, the closed loop).
    pub closed_loop: bool,
    /// Indices into `Program::rules`.
    pub rules: Vec<usize>,
    /// The relations its rules read, each once, with the polarity of the
    /// read (`+` positive, `-` negated, `~` inside an aggregation or reduction).
    pub reads: Vec<(String, char)>,
    /// Longest path from a primitive, recursion aside (primitives are 0).
    pub depth: usize,
}

/// The relations reachable from `decide`, in breadth-first order from it.
#[derive(Clone, Debug)]
pub struct ProgramGraph {
    pub nodes: Vec<Node>,
    pub by_name: HashMap<String, usize>,
    /// The longest path from a primitive to `decide`.
    pub depth: usize,
}

impl ProgramGraph {
    pub fn node(&self, name: &str) -> Option<&Node> {
        self.by_name.get(name).map(|&i| &self.nodes[i])
    }
}

fn kind_text(k: &Kind) -> &'static str {
    match k {
        Kind::Primitive { .. } => "primitive",
        Kind::Executor => "executor",
        Kind::KernelState => "kernel",
        Kind::Derived { .. } => "derived",
        Kind::Output => "output",
    }
}

fn polarity_char(p: Polarity) -> char {
    match p {
        Polarity::Positive => '+',
        Polarity::Negative => '-',
        Polarity::Aggregate => '~',
    }
}

/// Build the graph of the relations `decide` reaches.
pub fn program_graph(prog: &Program) -> ProgramGraph {
    // Reads per relation, from the checker's per-rule references.
    let mut reads: HashMap<&str, Vec<(String, char)>> = HashMap::new();
    let mut rules_of: HashMap<&str, Vec<usize>> = HashMap::new();
    for (i, rule) in prog.rules.iter().enumerate() {
        rules_of.entry(rule.head.name.as_str()).or_default().push(i);
        let entry = reads.entry(rule.head.name.as_str()).or_default();
        for r in &prog.infos[i].refs {
            let item = (r.name.clone(), polarity_char(r.polarity));
            if !entry.contains(&item) {
                entry.push(item);
            }
        }
    }
    // Breadth-first from decide.
    let mut order: Vec<String> = Vec::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut queue: VecDeque<String> = VecDeque::new();
    if prog.relations.contains_key("decide") {
        queue.push_back("decide".to_string());
        seen.insert("decide".to_string());
    }
    while let Some(name) = queue.pop_front() {
        order.push(name.clone());
        if let Some(rs) = reads.get(name.as_str()) {
            for (dep, _) in rs {
                if prog.relations.contains_key(dep) && seen.insert(dep.clone()) {
                    queue.push_back(dep.clone());
                }
            }
        }
    }
    let by_name: HashMap<String, usize> = order.iter().enumerate().map(|(i, n)| (n.clone(), i)).collect();
    // Edges within the graph, for the SCCs (recursion) and the depth.
    let n = order.len();
    let adj: Vec<Vec<usize>> = order
        .iter()
        .map(|name| {
            reads.get(name.as_str()).map(|rs| rs.iter().filter_map(|(d, _)| by_name.get(d).copied()).collect()).unwrap_or_default()
        })
        .collect();
    let sccs = tarjan(n, &adj);
    let mut scc_size: HashMap<usize, usize> = HashMap::new();
    for &id in &sccs.id {
        *scc_size.entry(id).or_default() += 1;
    }
    let self_edge: Vec<bool> = (0..n).map(|i| adj[i].contains(&i)).collect();
    // Depth: longest path over edges that leave the node's SCC (recursion
    // within an SCC is one node for this purpose), memoised; a node's depth is
    // 1 + the greatest depth among what it reads, and a primitive's is 0.
    let mut depth: Vec<Option<usize>> = vec![None; n];
    fn depth_of(i: usize, adj: &[Vec<usize>], sccs: &crate::check::Sccs, prog: &Program, order: &[String], depth: &mut Vec<Option<usize>>) -> usize {
        if let Some(d) = depth[i] {
            return d;
        }
        let derived = matches!(prog.relations[&order[i]].kind, Kind::Derived { .. } | Kind::Output);
        let mut d = 0;
        if derived {
            // Guard against a cycle the SCC filter did not remove (cannot happen; keeps the recursion finite).
            depth[i] = Some(0);
            for &j in &adj[i] {
                if sccs.id[j] != sccs.id[i] {
                    d = d.max(1 + depth_of(j, adj, sccs, prog, order, depth));
                } else {
                    d = d.max(1);
                }
            }
        }
        depth[i] = Some(d);
        d
    }
    for i in 0..n {
        depth_of(i, &adj, &sccs, prog, &order, &mut depth);
    }
    // Closed loop: reaches an executor or kernel-state relation.
    let mut closed: Vec<bool> = order
        .iter()
        .map(|name| matches!(prog.relations[name].kind, Kind::Executor | Kind::KernelState))
        .collect();
    // Fixpoint over the reads (the graph is small).
    loop {
        let mut changed = false;
        for i in 0..n {
            if !closed[i] && adj[i].iter().any(|&j| closed[j]) {
                closed[i] = true;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    let stratum_of = |name: &str| prog.strata.iter().position(|s| s.iter().any(|r| r == name));
    let nodes: Vec<Node> = order
        .iter()
        .enumerate()
        .map(|(i, name)| {
            let sig = &prog.relations[name];
            let args: Vec<String> = sig
                .args
                .iter()
                .map(|a| {
                    let m = match a.mode {
                        Mode::In => "+",
                        Mode::Out => "-",
                        Mode::Key => "@",
                    };
                    format!("{}{}: {}", m, a.name, a.ty)
                })
                .collect();
            Node {
                name: name.clone(),
                kind: kind_text(&sig.kind),
                signature: format!("{}({})", name, args.join(", ")),
                resolution: sig.res.map(|r| r.to_string()).unwrap_or_else(|| prog.resolution.to_string()),
                complete: prog.complete.contains(name),
                stratum: stratum_of(name),
                recursive: scc_size[&sccs.id[i]] > 1 || self_edge[i],
                closed_loop: closed[i],
                rules: rules_of.get(name.as_str()).cloned().unwrap_or_default(),
                reads: reads.get(name.as_str()).cloned().unwrap_or_default(),
                depth: depth[i].unwrap_or(0),
            }
        })
        .collect();
    let depth = nodes.iter().map(|x| x.depth).max().unwrap_or(0);
    ProgramGraph { nodes, by_name, depth }
}

/// The program as a reader sees it: the strategy's header, its parameters,
/// then every relation `decide` reaches, from `decide` upward, each with its
/// signature, kind, resolution, completeness, stratum, loop, depth, what it
/// reads and its rules; the primitives last.
pub fn show_program(prog: &Program) -> String {
    let g = program_graph(prog);
    let mut out = String::new();
    let count = |k: &str| g.nodes.iter().filter(|x| x.kind == k).count();
    out.push_str(&format!(
        "program {}: env {}, decision resolution {}, mode {}; {} relations reachable from decide ({} primitive, {} executor, {} kernel, {} derived, 1 output), {} rules, depth {}\n",
        prog.strategy,
        prog.environment,
        prog.resolution,
        prog.mode,
        g.nodes.len(),
        count("primitive"),
        count("executor"),
        count("kernel"),
        count("derived"),
        prog.rules.len(),
        g.depth
    ));
    if let Some(ps) = prog.params.get(&prog.strategy) {
        let shown: Vec<String> = ps
            .values()
            .map(|p| match &p.range {
                Some((lo, hi)) => format!("{} : {} = {} in {}..{}", p.name, p.ty, p.value, lo, hi),
                None => format!("{} : {} = {}", p.name, p.ty, p.value),
            })
            .collect();
        if !shown.is_empty() {
            out.push_str(&format!("params: {}\n", shown.join("; ")));
        }
    }
    out.push('\n');
    for node in g.nodes.iter().filter(|x| x.kind != "primitive") {
        let mut tags: Vec<String> = vec![node.kind.to_string(), node.resolution.clone()];
        if node.complete {
            tags.push("complete".into());
        }
        if let Some(s) = node.stratum {
            tags.push(format!("stratum {}", s));
        }
        if node.recursive {
            tags.push("recursive (reads its own past)".into());
        }
        tags.push(if node.closed_loop { "closed loop (reads the book)".into() } else { "open loop".into() });
        tags.push(format!("depth {}", node.depth));
        out.push_str(&format!("{}  [{}]\n", node.signature, tags.join(", ")));
        if !node.reads.is_empty() {
            let reads: Vec<String> = node.reads.iter().map(|(r, p)| format!("{}{}", r, p)).collect();
            out.push_str(&format!("  reads: {}\n", reads.join(", ")));
        }
        for &ri in &node.rules {
            let rule = &prog.rules[ri];
            let body: Vec<String> = rule.body.iter().map(|l| l.describe()).collect();
            out.push_str(&format!("  {}: {} :- {}.\n", prog.rule_label(ri), rule.head, body.join(", ")));
        }
    }
    let prims: Vec<&Node> = g.nodes.iter().filter(|x| x.kind == "primitive").collect();
    if !prims.is_empty() {
        out.push_str("\nprimitives read (from the environment):\n");
        for p in prims {
            out.push_str(&format!("  {}  [{}{}]\n", p.signature, p.resolution, if p.complete { ", complete: a missing tuple is false" } else { ", not complete: a missing tuple is unknown" }));
        }
    }
    out.push_str("\nlegend: a read's sign is + positive, - negated, ~ inside an aggregation or reduction; depth is the longest path from a primitive; a closed-loop relation is evaluated after the previous bar's fills, an open-loop one could be computed ahead of any decision\n");
    out
}

/// One empty link on a path from `decide`: the relation, and the path of
/// empty derived relations from `decide` down to it.
#[derive(Clone, Debug, PartialEq)]
pub struct RootCause {
    pub relation: String,
    pub path: Vec<String>,
}

/// The coverage report of a run: every derived relation `decide` reaches
/// with its counts, the relations demanded and never derived, and, when
/// `decide` derived nothing, the root causes: the empty relations on the
/// paths from `decide` whose own derived reads were not empty.
pub fn coverage_report(prog: &Program, coverage: &[RelationCoverage], bars: usize) -> (String, Vec<RootCause>) {
    let g = program_graph(prog);
    let by_name: HashMap<&str, &RelationCoverage> = coverage.iter().map(|c| (c.name.as_str(), c)).collect();
    let mut out = String::new();
    out.push_str(&format!("coverage over {} decision bars (calls: bars and inputs the relation was demanded at; tuples: what it derived):\n", bars));
    let width = g.nodes.iter().map(|n| n.name.len()).max().unwrap_or(8).max(8);
    let is_empty = |name: &str| -> bool {
        match (g.node(name), by_name.get(name)) {
            (Some(n), Some(c)) => n.kind != "primitive" && c.calls > 0 && c.tuples == 0,
            _ => false,
        }
    };
    // From decide upward (the graph's order), derived first.
    for node in g.nodes.iter().filter(|n| n.kind != "primitive" && n.kind != "executor" && n.kind != "kernel") {
        let c = by_name.get(node.name.as_str());
        let (calls, nonempty, tuples) = c.map(|c| (c.calls, c.nonempty, c.tuples)).unwrap_or((0, 0, 0));
        let mark = if calls == 0 {
            "   never demanded"
        } else if tuples == 0 {
            "   EMPTY: demanded, never derived"
        } else {
            ""
        };
        out.push_str(&format!("  {:<width$}  {:>8} calls  {:>8} with tuples  {:>10} tuples{}\n", node.name, calls, nonempty, tuples, mark, width = width));
    }
    for node in g.nodes.iter().filter(|n| n.kind == "primitive" || n.kind == "executor" || n.kind == "kernel") {
        if let Some(c) = by_name.get(node.name.as_str()) {
            let mark = if c.tuples == 0 { "   EMPTY: no tuple at all" } else { "" };
            let source = match node.kind {
                "primitive" => "in the data",
                "executor" => "from the executor",
                _ => "from the kernel",
            };
            out.push_str(&format!("  {:<width$}  {:>10} tuples {} ({}){}\n", node.name, c.tuples, source, node.kind, mark, width = width));
        }
    }
    let mut causes: Vec<RootCause> = Vec::new();
    if is_empty("decide") {
        out.push_str(&format!("never fired: decide derived no tuple over {} bars.\n", bars));
        // Depth-first from decide through empty derived relations; the leaves are the root causes.
        let mut path: Vec<String> = Vec::new();
        let mut visited: BTreeSet<String> = BTreeSet::new();
        fn walk(g: &ProgramGraph, name: &str, is_empty: &dyn Fn(&str) -> bool, path: &mut Vec<String>, visited: &mut BTreeSet<String>, causes: &mut Vec<RootCause>) {
            path.push(name.to_string());
            visited.insert(name.to_string());
            let node = g.node(name).unwrap();
            let empty_reads: Vec<&str> = node.reads.iter().map(|(r, _)| r.as_str()).filter(|r| is_empty(r)).collect();
            if empty_reads.is_empty() {
                causes.push(RootCause { relation: name.to_string(), path: path.clone() });
            } else {
                for r in empty_reads {
                    if !visited.contains(r) {
                        walk(g, r, is_empty, path, visited, causes);
                    }
                }
            }
            path.pop();
        }
        walk(&g, "decide", &is_empty, &mut path, &mut visited, &mut causes);
        for c in &causes {
            let node = g.node(&c.relation).unwrap();
            let reads: Vec<String> = node
                .reads
                .iter()
                .map(|(r, p)| {
                    let t = by_name.get(r.as_str()).map(|x| x.tuples).unwrap_or(0);
                    format!("{}{} ({} tuples)", r, p, t)
                })
                .collect();
            out.push_str(&format!(
                "  {}: `{}` is the first empty relation on this path; the relations it reads hold tuples ({}), so its rules fail on a test, a bound read (a label or a value none of them carries) or a window that never fills\n",
                c.path.join(" <- "),
                c.relation,
                if reads.is_empty() { "none".to_string() } else { reads.join(", ") }
            ));
        }
    } else {
        let empties: Vec<&str> = g.nodes.iter().filter(|n| n.kind == "derived" && is_empty(&n.name)).map(|n| n.name.as_str()).collect();
        if !empties.is_empty() {
            out.push_str(&format!("empty relations (demanded, never derived; a rule reading one positively never fires): {}\n", empties.join(", ")));
        }
    }
    (out, causes)
}
