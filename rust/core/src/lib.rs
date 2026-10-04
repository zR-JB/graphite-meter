//! Shared Graphite Meter protocol and measurement contracts.
#![forbid(unsafe_code)]

/// A fieldless enum whose variants are listed once, each with its row, and all of them in `ALL`.
macro_rules! vocabulary {
    (pub enum $name:ident -> $row:ty { $($variant:ident => $value:expr,)+ }) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum $name { $($variant,)+ }

        impl $name {
            pub const ALL: [Self; [$(stringify!($variant)),+].len()] = [$(Self::$variant),+];

            const fn row(self) -> $row {
                match self { $(Self::$variant => $value,)+ }
            }
        }
    };
}

/// A fieldless error enum whose variants are listed once, each with its message.
macro_rules! errors {
    (pub enum $name:ident { $($variant:ident => $message:expr,)+ }) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum $name { $($variant,)+ }

        impl std::fmt::Display for $name {
            fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str(match self { $(Self::$variant => $message,)+ })
            }
        }

        impl std::error::Error for $name {}
    };
}

pub mod approval;
pub mod catalog;
pub mod discovery;
pub mod duration;
pub mod failure;
pub mod format;
pub mod latency;
pub mod measurement;
pub mod origin;
pub mod route;
pub mod socket;
pub mod text;
pub mod wire;
