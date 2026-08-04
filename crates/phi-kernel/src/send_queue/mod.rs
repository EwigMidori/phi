//! SendQueue: per-session generation queue + pump (not "mailbox").
//!
//! Generation path: [`turn::GenerationTurn::handle`] → [`effect::EffectBatch`] →
//! [`effect::EffectApplier`]; [`turn::GenerationTurn::drive`] owns the stream;
//! open tools via [`tool_ledger::ToolLedger`]. Product injects
//! [`crate::agent::AgentPorts`] (runtime + [`crate::agent::TurnMaterials`]).
//! All transcript/bus commits go through [`effect::EffectApplier`] only.
//!
//! **Stream commit:** each write → record then projected tool notice; then batch
//! notices. Terminal jobs use [`effect::TurnOutcome`] via `apply_terminal`. See
//! [`effect`].

mod directory;
mod effect;
mod epoch;
mod job;
mod queue;
mod tool_ledger;
mod turn;

pub use directory::SessionDirectory;
pub use job::SendJob;
pub use queue::SendQueue;
