//! Times are nanoseconds (docs/spec/00-architecture.md, A-D7). A second-based
//! literal (`86_400`, `3600`) in a source outside the time modules is a
//! conversion the switch missed; a float formula that means seconds converts
//! its span with `time::secs_f64` on the same line.

use std::fs;
use std::path::{Path, PathBuf};

fn sources(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            sources(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

#[test]
fn no_second_literals_outside_the_time_modules() {
    let root = abt_tests::repo_root().join("crates");
    let mut files = Vec::new();
    for c in fs::read_dir(&root).unwrap() {
        let src = c.unwrap().path().join("src");
        if src.is_dir() {
            sources(&src, &mut files);
        }
    }
    let exempt = ["kernel/time.rs", "abt-wire/src/scalar.rs"];
    let pattern = ["86_400", "86400", "3_600", "3600"];
    let mut found = Vec::new();
    for f in files {
        let path = f.to_string_lossy().replace('\\', "/");
        if exempt.iter().any(|e| path.ends_with(e)) {
            continue;
        }
        for (i, line) in fs::read_to_string(&f).unwrap().lines().enumerate() {
            let code = line.split("//").next().unwrap_or("");
            if code.contains("secs_f64") || code.contains("86_400.0") {
                continue;
            }
            for p in pattern {
                if let Some(at) = code.find(p) {
                    let before = code[..at].chars().last();
                    let after = code[at + p.len()..].chars().next();
                    let digit = |c: Option<char>| c.is_some_and(|c| c.is_ascii_digit() || c == '_' || c == '.');
                    if !digit(before) && !digit(after) {
                        found.push(format!("{}:{}: {}", path, i + 1, line.trim()));
                    }
                }
            }
        }
    }
    assert!(
        found.is_empty(),
        "second-based literals outside the time modules (use kernel::time's SEC, MIN, HOUR, DAY):\n{}",
        found.join("\n")
    );
}
