//! QUIC endpoints: what their connections and buffers hold of the budget.

mod budget;

pub use budget::{endpoint_bytes, floor_bytes, noq_floor};
