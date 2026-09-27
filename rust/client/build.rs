#![forbid(unsafe_code)]

#[path = "../legal_build.rs"]
mod legal;

fn main() {
    if let Err(error) = legal::embed(false) {
        panic!("Rust legal notice embedding failed: {error}");
    }
}
