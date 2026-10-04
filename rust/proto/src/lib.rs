//! Shared wire contracts: routes, messages, discovery types, codes and their parsers.

/// A fieldless enum listed once: its variants in table order as `ALL`, each with its row.
macro_rules! table {
    ($(#[$meta:meta])* pub enum $name:ident: $row:ty { $($variant:ident => $value:expr,)+ }) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum $name { $($variant,)+ }

        impl $name {
            /// Every variant, in table order.
            pub const ALL: &[Self] = &[$(Self::$variant),+];

            const fn row(self) -> $row {
                match self { $(Self::$variant => $value,)+ }
            }
        }
    };
}

pub mod bus;
pub mod json;
pub mod lane;
pub mod reason;
pub mod refusal;
pub mod route;
pub mod upload;
