//! Opaque string newtype IDs for the kernel.
//!
//! Construction:
//! - **New ids:** UUID v4 via `generate`
//! - **External / tests:** [`FromStr`] / `.parse()`
//! - **Persistence rehydrate:** [`trust_from_persisted`] (store adapters only)
//!
//! **Forbidden:** public `From<String>`, letter-prefix counters.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::KernelError;

macro_rules! string_id {
    (
        $(#[$meta:meta])*
        pub struct $name:ident;
        empty = $empty:expr;
        generate as $gen:ident;
    ) => {
        $(#[$meta])*
        #[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }

            #[must_use]
            pub fn $gen() -> Self {
                Self(Uuid::new_v4().to_string())
            }

            /// Store rehydrate only — delegates to [`FromStr`] (single validation path).
            pub fn trust_from_persisted(s: impl AsRef<str>) -> Result<Self, KernelError> {
                s.as_ref().parse()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl FromStr for $name {
            type Err = KernelError;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                let s = s.trim();
                if s.is_empty() {
                    return Err(KernelError::InvalidArgument($empty.into()));
                }
                Ok(Self(s.to_owned()))
            }
        }
    };
}

string_id! {
    /// Live chat / generation unit identity.
    pub struct SessionId;
    empty = "session id empty";
    generate as generate;
}

string_id! {
    /// Immutable image bytes owned and resolved by the host.
    pub struct ImageId;
    empty = "image id empty";
    generate as generate;
}

string_id! {
    /// Transcript message identity.
    pub struct MessageId;
    empty = "message id empty";
    generate as generate;
}

string_id! {
    /// In-process generation job id (SendQueue, not transcript PK).
    pub struct JobId;
    empty = "job id empty";
    generate as generate;
}

string_id! {
    /// One complete provider response within a generation.
    pub struct ModelResponseId;
    empty = "model response id empty";
    generate as generate;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_str_rejects_empty() {
        assert!("".parse::<SessionId>().is_err());
        assert!("   ".parse::<JobId>().is_err());
    }

    #[test]
    fn generate_is_uuid() {
        let s = SessionId::generate();
        assert!(Uuid::parse_str(s.as_str()).is_ok());
    }
}
