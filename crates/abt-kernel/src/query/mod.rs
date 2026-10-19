//! The query language (V-8, K-7.2) over a run's state Σ_t: `? BODY at|for|over
//! ...`, `why R(...)`, `why not R(...) [in written order]`, `trace R at T`,
//! `coverage over W`, `show program|plan|schedule|calendars|contract` and
//! `ledger R into NAME`. A `?` query is a rule of a stratum above the
//! program's (Definition V-8.1): its body is checked with the program as
//! any rule is, the run holds it beside the program's rules, and it
//! changes no decision (Proposition V-8.4).

mod answer;
mod parse;

pub use answer::{answer, prepare, schedule, Answer, Context, Prepared, PREFIX};
pub use parse::{parse, Ask, Query, Show};
