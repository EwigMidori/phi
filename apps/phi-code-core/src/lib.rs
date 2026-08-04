//! # phi-code-core
//!
//! Product runtime for phi-code. Modules land one at a time.

#![forbid(unsafe_code)]

pub const CORE_NAME: &str = "phi-code-core";

pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
