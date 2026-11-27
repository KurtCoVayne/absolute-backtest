//! Hands `abt --version` the build's identity as `ABT_BUILD`
//! (`scripts/build_id.rs`, shared with the data layer's program).

#[path = "../../scripts/build_id.rs"]
mod build_id;

fn main() {
    build_id::emit();
}
