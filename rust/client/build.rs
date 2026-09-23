#[path = "../legal_build.rs"]
mod legal;

fn main() {
    if let Err(error) = legal::embed() {
        panic!("Rust legal notice embedding failed: {error}");
    }
}
