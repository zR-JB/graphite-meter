//! Embeds the third-party notices.

fn main() {
    if let Err(error) = graphite_meter_legal::embed(false) {
        panic!("client build inputs: {error}");
    }
}
