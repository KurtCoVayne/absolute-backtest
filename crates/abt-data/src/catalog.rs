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
    CatalogRelation { name, args, complete, note }
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
    vec![
        ClassDef {
            name: "equity",
            header: vec![
                "The equity class, as `abt bundle env` generates it from a bundle's manifest",
                "(docs/language-v2.md, 3.2 and 3.6): the primitives at the base calendar,",
                "the catalog relations, and the vocabularies. A program never names this",
                "file; its `data` block names what it needs and the kernel resolves it.",
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
                    relations: vec![
                        rel("listed", vec![arg("A", "Equity"), at("T")], true, Some("listed and tradable on T")),
                        rel("ticker", vec![arg("A", "Equity"), at("T"), arg("S", "Label")], true, None),
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
                        rel("delisted", vec![arg("A", "Equity"), at("T"), arg("Reason", "Label")], true, Some("a status from the delisting bar on")),
                        rel("member", vec![arg("A", "Equity"), at("T"), arg("List", "Label")], true, None),
                        rel("classification", vec![arg("A", "Equity"), at("T"), arg("Scheme", "Label"), arg("Code", "Label")], true, None),
                    ],
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
            ],
            vocabulary_comment: None,
            vocabularies: vec![Vocabulary {
                name: "lists",
                values: vec![("HOME-26", None), ("HOLD-6", None)],
            }],
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
            let mut line = format!("  {}({})", r.name, args.join(", "));
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
        name: r.name.to_string(),
        class: class.name.to_string(),
        columns,
        calendar: class.base.map(|(b, _)| b.to_string()),
        constraints: None,
        attributes: (!attributes.is_empty()).then_some(attributes),
        schema_version: Some(SCHEMA_VERSION.to_string()),
    }
}

/// Every schema the built-in classes define.
pub fn builtin_schemas() -> Vec<RelationSchema> {
    let mut out = Vec::new();
    for c in classes() {
        for g in &c.groups {
            for r in &g.relations {
                out.push(relation_schema(&c, r));
            }
        }
    }
    out
}
