//! Hands `abt-data --version` the build's identity as `ABT_BUILD`
//! (`scripts/build_id.rs`, shared with the front).

#[path = "../../scripts/build_id.rs"]
mod build_id;

fn main() {
    build_id::emit();
}
