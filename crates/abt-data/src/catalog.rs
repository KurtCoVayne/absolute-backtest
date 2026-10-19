//! The class catalogs (D-2.2, D-2.3): the registry's built-in classes and the
//! relation schemas they define, and their rendering as the class files the
//! kernel reads (`corpus-v2/catalog/*.dsl`; `abt data catalog --class C`,
//! V-3.2). The registry is the source; a class file is a rendering of it.

use abt_wire::generated::abt::wire::{Attribute, ColumnSpec, ColumnType, RelationSchema, Role};

/// The version of the schemas this registry writes (P-7).
pub const SCHEMA_VERSION: &str = "1.0";

/// A column of a catalog relation: its name in the class file and its type
/// as the language writes it (`Equity`, `Price<USD>`, `Label`, `Timestamp`).
#[derive(Clone, Debug, PartialEq)]
pub struct Arg {
    pub name: &'static str,
    pub ty: &'static str,
    /// The relation's event time (`@T`, or `@S` for sessions).
    pub time: bool,
}

/// A relation of a class.
#[derive(Clone, Debug, PartialEq)]
pub struct CatalogRelation {
    pub name: &'static str,
    pub args: Vec<Arg>,
    /// Declared complete (V-3.2): every true fact of the class is present.
    pub complete: bool,
    /// A trailing comment.
    pub note: Option<&'static str>,
    /// The class that stores it, when another class's programs read it too
    /// (the option class's underlyings are the equity class's bars and
    /// actions): the class file lists it, the store keeps it once, under its
    /// own class (ERRATA E-70).
    pub of: Option<&'static str>,
    /// Stored by this class, and read by programs through the kernel's
    /// relation of the same name (K-2.4: `greeks`, these rows under `greeks
    /// data`, the model's otherwise): the class file shows it as a comment,
    /// so that no program reads it as a primitive.
    pub served: bool,
}

/// A group of relations under a comment line.
#[derive(Clone, Debug, PartialEq)]
pub struct Group {
    pub comment: Option<&'static str>,
    pub relations: Vec<CatalogRelation>,
}

/// A vocabulary: the labels a label column takes (WF-15), with a native
/// calendar per name for the series class.
#[derive(Clone, Debug, PartialEq)]
pub struct Vocabulary {
    pub name: &'static str,
    pub values: Vec<(&'static str, Option<&'static str>)>,
}

/// A class of the registry.
#[derive(Clone, Debug, PartialEq)]
pub struct ClassDef {
    pub name: &'static str,
    pub header: Vec<&'static str>,
    pub base: Option<(&'static str, Option<&'static str>)>,
    pub unit: Option<&'static str>,
    pub entity_sort: &'static str,
    pub groups: Vec<Group>,
    /// A line between the relations and the vocabularies, when the file has one.
    pub vocabulary_comment: Option<&'static str>,
    pub vocabularies: Vec<Vocabulary>,
}

fn arg(name: &'static str, ty: &'static str) -> Arg {
    Arg { name, ty, time: false }
}

fn at(name: &'static str) -> Arg {
    Arg { name, ty: "Timestamp", time: true }
}

fn rel(name: &'static str, args: Vec<Arg>, complete: bool, note: Option<&'static str>) -> CatalogRelation {
    CatalogRelation {
        name,
        args,
        complete,
        note,
        of: None,
        served: false,
    }
}

/// Another class's relations, as this class's programs read them.
fn borrowed(class: &'static str, relations: Vec<CatalogRelation>) -> Vec<CatalogRelation> {
    relations.into_iter().map(|r| CatalogRelation { of: Some(class), ..r }).collect()
}

/// The classes the registry defines (D-2.3), in the form the corpus's
/// catalog files have.
pub fn classes() -> Vec<ClassDef> {
    let bars = |sort: &'static str, unit: &'static str| -> Vec<CatalogRelation> {
        vec![
            rel("open", vec![arg("A", sort), at("T"), arg("O", "Price<USD>")], false, None),
            rel("high", vec![arg("A", sort), at("T"), arg("H", "Price<USD>")], false, None),
            rel("low", vec![arg("A", sort), at("T"), arg("L", "Price<USD>")], false, None),
            rel("close", vec![arg("A", sort), at("T"), arg("P", "Price<USD>")], false, None),
            rel("volume", vec![arg("A", sort), at("T"), arg("V", unit)], false, None),
        ]
    };
    // The corporate actions of an equity, as events.
    let actions = || -> Vec<CatalogRelation> {
        vec![
            rel(
                "split",
                vec![arg("A", "Equity"), at("T"), arg("Factor", "Scalar")],
                true,
                Some("effective at the ex-date T: new shares per old"),
            ),
            rel(
                "dividend",
                vec![arg("A", "Equity"), at("T"), arg("Ex", "Timestamp"), arg("Pay", "Timestamp"), arg("Amount", "Price<USD>")],
                true,
                Some("announced at T"),
            ),
            rel(
                "special_dividend",
                vec![arg("A", "Equity"), at("T"), arg("Ex", "Timestamp"), arg("Amount", "Price<USD>")],
                true,
                Some("a dividend OCC adjusts strikes for (K-5.5.5)"),
            ),
            rel("delisted", vec![arg("A", "Equity"), at("T"), arg("Reason", "Label")], true, Some("a status from the delisting bar on")),
        ]
    };
    let margin = |sort: &'static str, note: &'static str| -> CatalogRelation {
        rel(
            "margin",
            vec![arg("A", sort), at("T"), arg("Initial", "Notional<USD>"), arg("Maintenance", "Notional<USD>")],
            true,
            Some(note),
        )
    };
    vec![
        ClassDef {
            name: "equity",
            header: vec![
                "The equity class, as `abt data catalog --class equity` renders it from the",
                "registry (docs/language-v2.md, 3.2 and 3.6): the primitives at the base",
                "calendar, the catalog relations, and the vocabularies. A program never",
                "names this file; its `data` block names what it needs and the kernel",
                "resolves it.",
            ],
            base: Some(("1d", Some("the finest resolution this bundle holds; a @1m bundle derives @1d by the bar views"))),
            unit: Some("shares"),
            entity_sort: "Equity",
            groups: vec![
                Group {
                    comment: Some("Bars as traded."),
                    relations: bars("Equity", "Quantity<Shares>"),
                },
                Group {
                    comment: Some("The catalog: listing and identity, corporate actions as events, point-in-time lists."),
                    relations: [
                        vec![
                            rel("listed", vec![arg("A", "Equity"), at("T")], true, Some("listed and tradable on T")),
                            rel("ticker", vec![arg("A", "Equity"), at("T"), arg("S", "Label")], true, None),
                        ],
                        actions(),
                        vec![
                            rel("member", vec![arg("A", "Equity"), at("T"), arg("List", "Label")], true, None),
                            rel("classification", vec![arg("A", "Equity"), at("T"), arg("Scheme", "Label"), arg("Code", "Label")], true, None),
                            margin("Equity", "per share, where the source publishes it (K-5.6)"),
                        ],
                    ]
                    .concat(),
                },
            ],
            vocabulary_comment: Some("Vocabularies (WF-15 checks a program's labels against them)."),
            vocabularies: vec![
                Vocabulary {
                    name: "lists",
                    values: vec![("SPX", None), ("SP500", None), ("SP400", None), ("SP600", None), ("ETF", None)],
                },
                Vocabulary {
                    name: "reasons",
                    values: vec![("bankruptcy", None), ("acquisition", None), ("voluntary", None), ("regulatory", None)],
                },
                Vocabulary {
                    name: "schemes",
                    values: vec![("sector", None)],
                },
            ],
        },
        ClassDef {
            name: "future",
            header: vec![
                "The future class: back-adjusted continuous contracts per root, whose prices",
                "may cross zero, with each root's own sessions, point value and commission",
                "per contract per side in the catalog.",
            ],
            base: Some(("1m", None)),
            unit: Some("contracts"),
            entity_sort: "Future",
            groups: vec![
                Group {
                    comment: None,
                    relations: bars("Future", "Quantity<Contracts>"),
                },
                Group {
                    comment: None,
                    relations: vec![
                        rel(
                            "session",
                            vec![arg("A", "Future"), at("S"), arg("Open", "Timestamp")],
                            true,
                            Some("one per root and session, labelled at the session's close"),
                        ),
                        rel("point_value", vec![arg("A", "Future"), at("T"), arg("V", "Scalar")], true, Some("currency per point per contract")),
                        rel("commission", vec![arg("A", "Future"), at("T"), arg("C", "Notional<USD>")], true, Some("per contract per side")),
                        rel("member", vec![arg("A", "Future"), at("T"), arg("List", "Label")], true, None),
                        rel("usable", vec![arg("A", "Future"), at("T")], true, Some("a research-defined availability, from the bundle's review")),
                    ],
                },
                Group {
                    comment: Some("Settlement, open interest and the margin an exchange publishes (K-5.4, K-5.6)."),
                    relations: vec![
                        rel(
                            "settlement",
                            vec![arg("A", "Future"), at("T"), arg("P", "Price<USD>")],
                            false,
                            Some("each session's settlement price, back-adjusted as the bars"),
                        ),
                        rel("open_interest", vec![arg("A", "Future"), at("T"), arg("N", "Quantity<Contracts>")], false, None),
                        margin("Future", "per contract"),
                    ],
                },
            ],
            vocabulary_comment: None,
            vocabularies: vec![Vocabulary {
                name: "lists",
                values: vec![("HOME-26", None), ("HOLD-6", None)],
            }],
        },
        ClassDef {
            name: "option",
            header: vec![
                "The option class: listed equity and index options, each contract an",
                "instrument with its terms in the registry's `option_spec` (the kernel's",
                "`spec`), quoted at the base calendar. A program's `need chain` names the",
                "contracts it reads (the kernel's `chain`); the underlyings' bars and",
                "actions are the equity class's.",
            ],
            base: Some(("1d", Some("end-of-day snapshots; an intraday source is a finer base"))),
            unit: Some("contracts"),
            entity_sort: "Option",
            groups: vec![
                Group {
                    comment: Some("Quotes as snapshotted, and the margin a source publishes per contract."),
                    relations: vec![
                        rel(
                            "quote",
                            vec![
                                arg("A", "Option"),
                                at("T"),
                                arg("Bid", "Price<USD>"),
                                arg("Ask", "Price<USD>"),
                                arg("BidSize", "Quantity<Contracts>"),
                                arg("AskSize", "Quantity<Contracts>"),
                                arg("Last", "Price<USD>"),
                                arg("Volume", "Quantity<Contracts>"),
                                arg("OI", "Quantity<Contracts>"),
                            ],
                            false,
                            None,
                        ),
                        CatalogRelation {
                            served: true,
                            ..rel(
                                "greeks",
                                vec![
                                    arg("A", "Option"),
                                    at("T"),
                                    arg("IV", "Scalar"),
                                    arg("Delta", "Scalar"),
                                    arg("Gamma", "Scalar"),
                                    arg("Theta", "Scalar"),
                                    arg("Vega", "Scalar"),
                                    arg("Rho", "Scalar"),
                                ],
                                false,
                                Some("the kernel's relation (K-2.4): these rows under `greeks data`, the model's under `greeks model`"),
                            )
                        },
                        margin("Option", "per contract, where the source publishes it (K-5.6)"),
                    ],
                },
                Group {
                    comment: Some("The underlyings: the equity class's bars and actions."),
                    relations: [borrowed("equity", bars("Equity", "Quantity<Shares>")), borrowed("equity", actions())].concat(),
                },
            ],
            vocabulary_comment: Some("Vocabularies of the kernel's `spec` (K-2.4)."),
            vocabularies: vec![
                Vocabulary {
                    name: "rights",
                    values: vec![("call", None), ("put", None)],
                },
                Vocabulary {
                    name: "styles",
                    values: vec![("american", None), ("european", None)],
                },
                Vocabulary {
                    name: "settlements",
                    values: vec![("physical", None), ("cash", None)],
                },
            ],
        },
        ClassDef {
            name: "series",
            header: vec![
                "Market series by name, each on its native calendar; read on a coarser or",
                "equal calendar as a bar view and on a finer one as of the bar (3.4).",
            ],
            base: None,
            unit: None,
            entity_sort: "Name",
            groups: vec![Group {
                comment: None,
                relations: vec![rel("value", vec![arg("Name", "Label"), at("T"), arg("V", "Scalar")], false, None)],
            }],
            vocabulary_comment: None,
            vocabularies: vec![Vocabulary {
                name: "names",
                values: vec![("SPXTR", Some("1d")), ("VIX", Some("1w")), ("VIX_MA4", Some("1w"))],
            }],
        },
        // The pipeline's provenance (absolute-pipeline PL-7.2; ERRATA E-94):
        // written by its gateway alone through `put` (P-3.1), like the
        // other layers' namespaces, and read with scan, series and catalog;
        // no program reads it. Ids are labels, since a row's identity is its
        // label columns with its time (D-2.2).
        ClassDef {
            name: "pipeline",
            header: vec![
                "The pipeline class (PL-7.2): the provenance of what the agents submit, from",
                "the document a hypothesis came from to the forward reveals of its",
                "submission, and the gateway's audit. Written by the gateway alone through",
                "`put`, like the kernel's and the query layer's namespaces; no program reads",
                "it. Ids are labels: a row's identity is its labels with its time (D-2.2).",
            ],
            base: None,
            unit: None,
            entity_sort: "Label",
            groups: vec![
                Group {
                    comment: Some("A document, the hypotheses it led to, and the submissions that test them."),
                    relations: vec![
                        rel(
                            "document",
                            vec![arg("DocId", "Label"), arg("Source", "Label"), arg("Author", "Label"), at("P"), arg("Size", "Count")],
                            true,
                            Some("published at P, Size bytes; the text stays in the ingest store"),
                        ),
                        rel(
                            "hypothesis",
                            vec![arg("Id", "Label"), arg("DocIds", "Label"), at("H"), arg("Agent", "Label")],
                            true,
                            Some("DocIds is the JSON list of its documents; H its horizon"),
                        ),
                        rel(
                            "submission",
                            vec![
                                arg("Id", "Label"),
                                arg("HypothesisId", "Label"),
                                arg("ProgramHash", "Label"),
                                arg("PlanHash", "Label"),
                                arg("Lineage", "Label"),
                                at("H"),
                            ],
                            true,
                            Some("written once, at submit (PL-6.6): H(P), H(Π), the lineage and H"),
                        ),
                    ],
                },
                Group {
                    comment: Some("The forward reveals of a submission, and the gateway's audit (PL-7.1)."),
                    relations: vec![
                        rel(
                            "outcome",
                            vec![
                                arg("SubmissionId", "Label"),
                                at("T"),
                                arg("Reveal", "Count"),
                                arg("ForwardFrom", "Timestamp"),
                                arg("ForwardTo", "Timestamp"),
                                arg("Status", "Label"),
                            ],
                            true,
                            Some("the Reveal-th reveal, at T, over (ForwardFrom, ForwardTo]; Status insample_only, forward or retired"),
                        ),
                        rel(
                            "audit",
                            vec![
                                arg("Job", "Label"),
                                at("T"),
                                arg("Kind", "Label"),
                                arg("Subject", "Label"),
                                arg("Bytes", "Count"),
                                arg("DurationNs", "Count"),
                                arg("Outcome", "Label"),
                            ],
                            true,
                            Some("one per request at T: its kind, the relation or plan, the bytes, the time it took"),
                        ),
                    ],
                },
            ],
            vocabulary_comment: None,
            vocabularies: vec![],
        },
    ]
}

/// The column a trailing comment starts at, as the class files align them.
const NOTE_COLUMN: usize = 68;
/// The column the base line's comment starts at.
const BASE_NOTE_COLUMN: usize = 33;

fn pad_to(line: &mut String, column: usize) {
    if line.chars().count() + 3 > column {
        line.push_str("   ");
    } else {
        while line.chars().count() < column {
            line.push(' ');
        }
    }
}

/// The class file of a class (V-3.2).
pub fn render_class(c: &ClassDef) -> String {
    let mut out = String::new();
    for h in &c.header {
        out.push_str(&format!("# {}\n", h));
    }
    out.push_str(&format!("class {} {{\n", c.name));
    if let Some((base, note)) = c.base {
        let mut line = format!("  base @{}", base);
        if let Some(n) = note {
            pad_to(&mut line, BASE_NOTE_COLUMN);
            line.push_str(&format!("# {}", n));
        }
        out.push_str(&line);
        out.push('\n');
    }
    if let Some(u) = c.unit {
        out.push_str(&format!("  unit {}\n", u));
    }
    for (gi, g) in c.groups.iter().enumerate() {
        if c.base.is_some() || c.unit.is_some() || gi > 0 {
            out.push('\n');
        }
        if let Some(comment) = g.comment {
            out.push_str(&format!("  # {}\n", comment));
        }
        for r in &g.relations {
            let args: Vec<String> = r.args.iter().map(|a| if a.time { format!("@{}", a.name) } else { format!("{}: {}", a.name, a.ty) }).collect();
            let mut line = format!("  {}{}({})", if r.served { "# " } else { "" }, r.name, args.join(", "));
            if r.complete {
                line.push_str(" complete");
            }
            if let Some(n) = r.note {
                pad_to(&mut line, NOTE_COLUMN);
                line.push_str(&format!("# {}", n));
            }
            out.push_str(&line);
            out.push('\n');
        }
    }
    if !c.vocabularies.is_empty() {
        if c.base.is_some() || c.unit.is_some() {
            out.push('\n');
        }
        if let Some(comment) = c.vocabulary_comment {
            out.push_str(&format!("  # {}\n", comment));
        }
        let width = c.vocabularies.iter().map(|v| v.name.len()).max().unwrap_or(0);
        for v in &c.vocabularies {
            let values: Vec<String> = v
                .values
                .iter()
                .map(|(name, cal)| match cal {
                    Some(cal) => format!("\"{}\" @{}", name, cal),
                    None => format!("\"{}\"", name),
                })
                .collect();
            let name = if c.vocabularies.len() > 1 {
                format!("{:width$}", v.name, width = width)
            } else {
                v.name.to_string()
            };
            out.push_str(&format!("  {} {}\n", name, values.join(", ")));
        }
    }
    out.push_str("}\n");
    out
}

/// The scalar type a language type is stored as (D-2.1).
pub fn column_type(ty: &str) -> ColumnType {
    match ty {
        "Equity" | "Future" | "Option" => ColumnType::Entity,
        "Timestamp" => ColumnType::Time,
        "Label" => ColumnType::Label,
        "Count" => ColumnType::Count,
        "Bool" => ColumnType::Bool,
        _ => ColumnType::Num,
    }
}

/// The schema of a catalog relation (Definition D-2.1): its arguments as
/// columns (the instrument, or the series' name, is the entity; the time
/// argument is the event time), then the receipt time every stored row
/// carries (D-4.2: `r = t` when its source has none).
pub fn relation_schema(class: &ClassDef, r: &CatalogRelation) -> RelationSchema {
    let mut columns: Vec<ColumnSpec> = r
        .args
        .iter()
        .enumerate()
        .map(|(i, a)| {
            let ty = column_type(a.ty);
            let role = if a.time {
                Role::EventTime
            } else if i == 0 && (ty == ColumnType::Entity || class.name == "series") {
                Role::Entity
            } else if ty == ColumnType::Label {
                Role::Label
            } else {
                Role::Value
            };
            ColumnSpec {
                name: a.name.to_string(),
                ty,
                role,
                dimension: (ty == ColumnType::Num).then(|| a.ty.to_string()),
            }
        })
        .collect();
    columns.push(ColumnSpec {
        name: "r".to_string(),
        ty: ColumnType::Time,
        role: Role::ReceiptTime,
        dimension: None,
    });
    let mut attributes = Vec::new();
    if r.complete {
        attributes.push(Attribute {
            key: "complete".into(),
            value: "true".into(),
        });
    }
    RelationSchema {
        name: qualified(class.name, r.name),
        class: class.name.to_string(),
        columns,
        calendar: class.base.map(|(b, _)| b.to_string()),
        constraints: None,
        attributes: (!attributes.is_empty()).then_some(attributes),
        schema_version: Some(SCHEMA_VERSION.to_string()),
    }
}

/// A market class's relation as the store names it, `<class>.<relation>`
/// (D-2.2: the registry holds a schema by class and name; `close` is the
/// equity class's and the future class's).
pub fn qualified(class: &str, relation: &str) -> String {
    format!("{}.{}", class, relation)
}

/// Every schema the built-in classes define.
pub fn builtin_schemas() -> Vec<RelationSchema> {
    let mut out = Vec::new();
    for c in classes() {
        for g in &c.groups {
            for r in g.relations.iter().filter(|r| r.of.is_none()) {
                out.push(relation_schema(&c, r));
            }
        }
    }
    out
}

/// The store's name of a relation a class's programs read: the class's
/// own, or the class it is borrowed from (`close` of the option class is
/// `equity.close`).
pub fn store_name(class: &str, relation: &str) -> String {
    let owner = classes()
        .iter()
        .find(|c| c.name == class)
        .and_then(|c| c.groups.iter().flat_map(|g| &g.relations).find(|r| r.name == relation))
        .and_then(|r| r.of)
        .unwrap_or(class);
    qualified(owner, relation)
}
