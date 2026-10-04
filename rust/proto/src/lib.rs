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

/// A `table!` enum whose row is its wire name, which it serializes as.
macro_rules! named {
    ($(#[$meta:meta])* pub enum $name:ident { $($variant:ident => $value:expr,)+ }) => {
        table! { $(#[$meta])* pub enum $name: &'static str { $($variant => $value,)+ } }

        impl $name {
            pub const fn name(self) -> &'static str {
                self.row()
            }

            pub fn from_name(name: &str) -> Option<Self> {
                Self::ALL.iter().copied().find(|known| known.name() == name)
            }
        }

        impl serde::Serialize for $name {
            fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.serialize_str(self.name())
            }
        }

        impl<'de> serde::Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let name = String::deserialize(deserializer)?;
                Self::from_name(&name).ok_or_else(|| serde::de::Error::custom(format!("unknown value {name:?}")))
            }
        }
    };
}

pub mod approval;
pub mod bus;
pub mod catalog;
pub mod discovery;
pub mod duration;
pub mod flag;
pub mod idna;
pub mod json;
pub mod lane;
pub mod origin;
pub mod reason;
pub mod refusal;
pub mod route;
pub mod text;
pub mod token;
pub mod upload;
