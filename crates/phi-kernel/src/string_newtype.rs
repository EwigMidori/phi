//! Transparent `String` newtype macro (open labels).
//!
//! Distinct from [`crate::ids`] `string_id!`:
//! - **ids:** UUID `generate`, empty reject via `FromStr`, no public `From<String>`
//! - **here:** `new` + `From<&str>` / `From<String>`, optional `Ord` for map keys
//!
//! Domain types (`SkillSlug`, …) still live in their owning modules; this file only
//! supplies the boilerplate macro. Not part of the public crate API.

/// Transparent `String` newtype: `new` / `as_str` / `AsRef` / `Display` / `From`.
///
/// Use trailing `ordered` for map keys (`Ord` + `PartialOrd`).
macro_rules! string_newtype {
    (
        $(#[$meta:meta])*
        pub struct $name:ident
    ) => {
        string_newtype!(@def $(#[$meta])* pub struct $name; ordered: false);
    };
    (
        $(#[$meta:meta])*
        pub struct $name:ident;
        ordered
    ) => {
        string_newtype!(@def $(#[$meta])* pub struct $name; ordered: true);
    };
    (@def
        $(#[$meta:meta])*
        pub struct $name:ident;
        ordered: false
    ) => {
        $(#[$meta])*
        #[derive(
            Clone,
            Debug,
            Eq,
            PartialEq,
            Hash,
            ::serde::Serialize,
            ::serde::Deserialize,
        )]
        #[serde(transparent)]
        pub struct $name(String);

        string_newtype!(@impls $name);
    };
    (@def
        $(#[$meta:meta])*
        pub struct $name:ident;
        ordered: true
    ) => {
        $(#[$meta])*
        #[derive(
            Clone,
            Debug,
            Eq,
            PartialEq,
            Ord,
            PartialOrd,
            Hash,
            ::serde::Serialize,
            ::serde::Deserialize,
        )]
        #[serde(transparent)]
        pub struct $name(String);

        string_newtype!(@impls $name);
    };
    (@impls $name:ident) => {
        impl $name {
            #[must_use]
            pub fn new(value: impl Into<String>) -> Self {
                Self(value.into())
            }

            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                self.as_str()
            }
        }

        impl ::std::fmt::Display for $name {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                f.write_str(self.as_str())
            }
        }

        impl From<&str> for $name {
            fn from(value: &str) -> Self {
                Self::new(value)
            }
        }

        impl From<String> for $name {
            fn from(value: String) -> Self {
                Self::new(value)
            }
        }
    };
}
