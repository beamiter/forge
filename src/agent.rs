//! Review-first agent core, shared with the jterm app family through
//! `jterm_core`.
//!
//! The hardened session wrapper — a process-local task epoch, snapshot
//! invariant validation with transcript/proposal byte caps, live-outcome
//! command validation with auto-reject, and one-shot snapshot file claims —
//! lives in `jterm_core::agent`. This module re-exports that surface under
//! forge's historical `crate::agent` paths so call sites stay unchanged.
//! Forge-specific integration (pane binding, execution correlation, the
//! snapshot parent lock) lives in `ui::agent_panel`.

pub use jterm_core::agent::{
    is_auto_approvable, is_dangerous, parse_action, sample_observation, try_claim_session_file,
    write_snapshot_file, AgentSession, AgentSessionEpoch, AgentSessionSnapshot, AgentSnapshotError,
    AgentState, ApprovedCommand, CancellationToken, ModelOutcome, ParseError, ParsedAction,
    ProposalId, ProposalStatus, SessionClaim, SessionError, Turn, MAX_AGENT_SNAPSHOT_JSON_BYTES,
};

// Forge historically exposed these core helpers through `crate::agent`.
// Keep that source-compatible surface while all live restore code uses the
// typed, durability-owning claim above.
#[allow(deprecated)]
pub use jterm_core::agent::{claim_session_file, read_snapshot_file, remove_snapshot_file};
