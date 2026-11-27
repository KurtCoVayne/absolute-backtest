//! The build's identity (`scripts/build_id.rs`, the module of the two build
//! scripts, compiled here as a module of the test): which of its sources
//! names a build, what makes cargo run the script again, and that nothing
//! but the two `--version` flags reads it (ERRATA E-133). `cli.rs` holds
//! `abt --version` itself. The git cases make a repository of their own in a
//! temporary directory and are skipped, with a note, where `git` is absent.

#[allow(dead_code)]
#[path = "../../../scripts/build_id.rs"]
mod build_id;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use build_id::identity;

/// A fresh temporary directory for one test.
fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("abt-build-id-{}-{}", tag, std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn has_git() -> bool {
    Command::new("git").arg("--version").output().is_ok_and(|o| o.status.success())
}

/// `git ARGS` in `dir`, under no configuration but the test's own (a
/// committer, no signing, no hooks); what it printed.
fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=abt", "-c", "user.email=abt@localhost", "-c", "commit.gpgsign=false", "-c", "core.hooksPath=/dev/null"])
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("git runs");
    assert!(out.status.success(), "git {:?}: {}", args, String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim_end().to_string()
}

/// A repository at `dir` with one commit holding `tracked.txt`.
fn repository(dir: &Path) {
    git(dir, &["init", "-q"]);
    fs::write(dir.join("tracked.txt"), "one\n").unwrap();
    git(dir, &["add", "tracked.txt"]);
    git(dir, &["commit", "-q", "-m", "one"]);
}

/// Source 1: the commit the builder names wins over the tree's own git and
/// over `BUILD_COMMIT`, and then no file is watched; a name that is empty or
/// blank is no name.
#[test]
fn a_build_given_its_commit_is_named_by_it_before_git_and_the_file() {
    let dir = scratch("given");
    fs::write(dir.join("BUILD_COMMIT"), "from-the-file\n").unwrap();
    if has_git() {
        repository(&dir);
    }
    let found = identity(Some("0123456789ab\n"), &dir);
    assert_eq!(found.id, "0123456789ab");
    assert!(found.read.is_empty(), "{:?}", found.read);
    for blank in ["", "  ", "\n"] {
        assert_ne!(identity(Some(blank), &dir).id, "", "{:?}", blank);
        assert_eq!(identity(Some(blank), &dir).id, identity(None, &dir).id, "{:?}", blank);
    }
    let _ = fs::remove_dir_all(&dir);
}

/// Sources 3 and 4: a tree without `.git` is named by the first line of its
/// `BUILD_COMMIT`, which is then watched; without the file, or with an empty
/// one, it is `unknown`, and a file that does not exist is not watched.
#[test]
fn a_tree_without_git_is_named_by_the_first_line_of_build_commit_or_unknown() {
    let dir = scratch("file");
    let found = identity(None, &dir);
    assert_eq!(found.id, "unknown");
    assert!(found.read.is_empty(), "{:?}", found.read);
    let file = dir.join("BUILD_COMMIT");
    fs::write(&file, "  9c9271cafdc6  \nbuilt by hand\n").unwrap();
    let found = identity(None, &dir);
    assert_eq!(found.id, "9c9271cafdc6");
    assert_eq!(found.read, vec![file.clone()]);
    fs::write(&file, "\n").unwrap();
    let found = identity(None, &dir);
    assert_eq!(found.id, "unknown");
    assert_eq!(found.read, vec![file]);
    let _ = fs::remove_dir_all(&dir);
}

/// Source 2: a git tree is named by twelve digits of its commit, before its
/// `BUILD_COMMIT`; `-dirty` is added while a tracked file differs from the
/// commit, and an untracked file is not a difference.
#[test]
fn a_git_tree_is_named_by_its_commit_and_dirty_while_a_tracked_file_differs() {
    if !has_git() {
        eprintln!("skipped: git is not installed");
        return;
    }
    let dir = scratch("git");
    repository(&dir);
    fs::write(dir.join("BUILD_COMMIT"), "from-the-file\n").unwrap();
    let commit = git(&dir, &["rev-parse", "HEAD"]);
    let clean = identity(None, &dir).id;
    assert_eq!(clean.len(), 12, "{}", clean);
    assert!(commit.starts_with(&clean), "{} is not the start of {}", clean, commit);
    fs::write(dir.join("tracked.txt"), "two\n").unwrap();
    assert_eq!(identity(None, &dir).id, format!("{}-dirty", clean));
    git(&dir, &["commit", "-q", "-a", "-m", "two"]);
    let next = identity(None, &dir).id;
    assert!(
        next != clean && !next.ends_with("-dirty") && git(&dir, &["rev-parse", "HEAD"]).starts_with(&next),
        "{} after {}",
        next,
        clean
    );
    let _ = fs::remove_dir_all(&dir);
}

/// What the script watches in a git tree exists (cargo takes a missing file
/// as changed on every build, which would rebuild the crate every time) and
/// changes when `HEAD` moves: by a commit on the branch, by a commit after
/// the refs were packed, and by a checkout of another commit.
#[test]
fn the_files_a_git_trees_identity_was_read_from_change_when_head_moves() {
    if !has_git() {
        eprintln!("skipped: git is not installed");
        return;
    }
    let dir = scratch("watch");
    repository(&dir);
    let watched = |what: &str| -> Vec<(PathBuf, Vec<u8>)> {
        let read = identity(None, &dir).read;
        assert!(!read.is_empty(), "{}: nothing is watched", what);
        read.into_iter()
            .map(|p| (p.clone(), fs::read(&p).unwrap_or_else(|e| panic!("{}: {} is watched and is not a file: {}", what, p.display(), e))))
            .collect()
    };
    let changed = |before: &[(PathBuf, Vec<u8>)]| before.iter().any(|(p, bytes)| fs::read(p).ok().as_ref() != Some(bytes));
    let first = git(&dir, &["rev-parse", "HEAD"]);
    let before = watched("a new repository");
    assert!(!changed(&before));
    git(&dir, &["commit", "-q", "--allow-empty", "-m", "two"]);
    assert!(changed(&before), "a commit on the branch changed none of {:?}", before.iter().map(|(p, _)| p).collect::<Vec<_>>());
    git(&dir, &["pack-refs", "--all"]);
    let before = watched("packed refs");
    git(&dir, &["commit", "-q", "--allow-empty", "-m", "three"]);
    assert!(
        changed(&before),
        "a commit after the refs were packed changed none of {:?}",
        before.iter().map(|(p, _)| p).collect::<Vec<_>>()
    );
    let before = watched("before a checkout");
    git(&dir, &["checkout", "-q", "--detach", &first]);
    assert!(changed(&before), "a checkout changed none of {:?}", before.iter().map(|(p, _)| p).collect::<Vec<_>>());
    assert!(first.starts_with(&identity(None, &dir).id));
    let _ = fs::remove_dir_all(&dir);
}

/// A directory inside a git tree that is not itself one (a tree unpacked
/// under some other repository) is not named by that repository's commit.
#[test]
fn a_directory_inside_a_git_tree_is_not_named_by_that_trees_commit() {
    if !has_git() {
        eprintln!("skipped: git is not installed");
        return;
    }
    let dir = scratch("inside");
    repository(&dir);
    let inner = dir.join("unpacked");
    fs::create_dir_all(&inner).unwrap();
    assert_eq!(identity(None, &inner).id, "unknown");
    fs::write(inner.join("BUILD_COMMIT"), "9c9271cafdc6\n").unwrap();
    assert_eq!(identity(None, &inner).id, "9c9271cafdc6");
    let _ = fs::remove_dir_all(&dir);
}

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

/// E-133: nothing of the make enters a run's outputs, its run id or a
/// digest. The identity reaches a crate's sources only as the compile-time
/// variable `ABT_BUILD`, and the only sources that name it are the two
/// programs' entry files, which print it for `--version`; the two crates
/// with a build script are the only ones it is defined for.
#[test]
fn nothing_but_the_two_version_flags_reads_the_builds_identity() {
    let crates = abt_tests::repo_root().join("crates");
    let mut readers = Vec::new();
    let mut scripts = Vec::new();
    for c in fs::read_dir(&crates).unwrap() {
        let c = c.unwrap().path();
        if c.join("build.rs").is_file() && fs::read_to_string(c.join("build.rs")).unwrap().contains("build_id") {
            scripts.push(c.file_name().unwrap().to_string_lossy().to_string());
        }
        if c.join("src").is_dir() {
            let mut files = Vec::new();
            sources(&c.join("src"), &mut files);
            for f in files {
                if fs::read_to_string(&f).unwrap().contains("ABT_BUILD") {
                    readers.push(f.strip_prefix(&crates).unwrap().to_string_lossy().replace('\\', "/"));
                }
            }
        }
    }
    readers.sort();
    scripts.sort();
    assert_eq!(readers, ["abt-data/src/bin/abt-data.rs", "abt/src/main.rs"]);
    assert_eq!(scripts, ["abt", "abt-data"]);
}
