//! The build's identity: what `abt --version` and `abt-data --version` print
//! in parentheses, so that a deployment can say which commit serves. One
//! implementation, the module of both build scripts (`crates/abt/build.rs`,
//! `crates/abt-data/build.rs`), which hand it to their programs as the
//! compile-time variable `ABT_BUILD`.
//!
//! The identity is the first of:
//!
//! 1. `ABT_BUILD_COMMIT`, when set and not empty: the builder names the
//!    commit (a tree sent to a machine without its `.git`);
//! 2. `git rev-parse --short=12 HEAD` of the workspace, with `-dirty` when a
//!    tracked file differs from that commit;
//! 3. the first line of the file `BUILD_COMMIT` at the workspace root (an
//!    exported tree);
//! 4. `unknown`.
//!
//! It builds without `.git` and without `git`. Cargo runs the script again
//! when `ABT_BUILD_COMMIT` changes or a file the answer was read from does
//! (the files `HEAD` moves through; `BUILD_COMMIT`), not on every build:
//! `-dirty` is therefore the tree's state when the script last ran, and an
//! edit made after it under the same `HEAD` does not add the mark. A
//! deployment's build names its commit with `ABT_BUILD_COMMIT`.
//!
//! Nothing of the make enters a run's outputs, its run id or a digest
//! (ERRATA E-133): the two `--version` flags are the identity's only readers
//! (`crates/abt/tests/build_id.rs` holds that), and the kernel and the query
//! layer have no build script, so they cannot read it.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// What a build is called, and the files the answer was read from: the build
/// script is run again when one of them changes.
pub struct Identity {
    pub id: String,
    pub read: Vec<PathBuf>,
}

/// The first line of `text`, when it holds anything.
fn first_line(text: &str) -> Option<String> {
    let line = text.lines().next().unwrap_or("").trim();
    (!line.is_empty()).then(|| line.to_string())
}

/// The variables that name git a repository, its work tree, its index and
/// its objects in place of the ones it finds from the directory. Git exports
/// them to a hook, so a build or a test started from one would otherwise ask
/// (and the tests' own commits would write) the repository the hook belongs
/// to, whatever directory `-C` names.
pub const REPOSITORY_VARIABLES: [&str; 6] = [
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_COMMON_DIR",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
];

/// What `git ARGS` prints in the tree at `root`, found from `root` alone
/// (`REPOSITORY_VARIABLES` are not passed on); `None` when there is no `git`
/// or it refuses. It takes no optional lock, so asking for the tree's status
/// never rewrites the index of a tree someone works in.
fn git(root: &Path, args: &[&str]) -> Option<String> {
    let mut git = Command::new("git");
    git.arg("-C").arg(root).args(args).env("GIT_OPTIONAL_LOCKS", "0").stderr(Stdio::null());
    for name in REPOSITORY_VARIABLES {
        git.env_remove(name);
    }
    let out = git.output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim_end().to_string())
}

/// The commit of the tree at `root` and the files `HEAD` moves through, when
/// `root` is itself a git tree (a directory inside some other repository is
/// not named by that repository's commit).
fn git_identity(root: &Path) -> Option<Identity> {
    if !root.join(".git").exists() {
        return None;
    }
    let commit = first_line(&git(root, &["rev-parse", "--short=12", "HEAD"])?)?;
    // A status git does not give is not a clean tree.
    let dirty = git(root, &["status", "--porcelain", "--untracked-files=no"]).is_none_or(|s| !s.is_empty());
    // `HEAD` names the branch and its log gains a line whenever the commit
    // under it changes; the branch's own file, the packed refs and the
    // reftable's list cover a repository that keeps no log. In a worktree
    // `.git` is a file and these live in the repository it names, so git is
    // asked where each one is; a file that does not exist is not watched
    // (cargo takes a missing file as changed on every build).
    let mut names: Vec<String> = ["HEAD", "logs/HEAD", "packed-refs", "reftable/tables.list"].iter().map(|n| n.to_string()).collect();
    names.extend(git(root, &["symbolic-ref", "-q", "HEAD"]).as_deref().and_then(first_line));
    let read = names
        .iter()
        .filter_map(|name| git(root, &["rev-parse", "--git-path", name]).as_deref().and_then(first_line))
        .map(|path| root.join(path))
        .filter(|path| path.exists())
        .collect();
    Some(Identity {
        id: if dirty { format!("{}-dirty", commit) } else { commit },
        read,
    })
}

/// The identity of a build of the workspace at `root`, `named` being what
/// `ABT_BUILD_COMMIT` holds.
pub fn identity(named: Option<&str>, root: &Path) -> Identity {
    if let Some(id) = named.and_then(first_line) {
        return Identity { id, read: Vec::new() };
    }
    if let Some(found) = git_identity(root) {
        return found;
    }
    let file = root.join("BUILD_COMMIT");
    match std::fs::read_to_string(&file) {
        Ok(text) => Identity {
            id: first_line(&text).unwrap_or_else(|| "unknown".to_string()),
            read: vec![file],
        },
        Err(_) => Identity {
            id: "unknown".to_string(),
            read: Vec::new(),
        },
    }
}

/// The build script of a crate of the workspace (`crates/NAME`): `ABT_BUILD`
/// for the crate's programs, and what cargo watches to run the script again.
pub fn emit() {
    println!("cargo:rerun-if-env-changed=ABT_BUILD_COMMIT");
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR is set by cargo"));
    let root = manifest.parent().and_then(Path::parent).expect("a crate of the workspace sits two levels below its root");
    let found = identity(std::env::var("ABT_BUILD_COMMIT").ok().as_deref(), root);
    for path in &found.read {
        println!("cargo:rerun-if-changed={}", path.display());
    }
    println!("cargo:rustc-env=ABT_BUILD={}", found.id);
}
