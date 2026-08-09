//! Native GTK shell Agent UI. The model can only propose commands; every
//! command remains editable and requires an explicit per-command approval.
//!
//! The session renders as an inline card in the bound Block pane's
//! conversation, pinned directly above the live prompt: activity, proposals,
//! approval, and the instruction composer all live in the block flow.
//! Configuration-type content (identity, provider chips, the correction
//! toggle) stays in a small settings dialog opened from the card header.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet, VecDeque};
use std::rc::{Rc, Weak};
#[cfg(unix)]
use std::sync::atomic::{AtomicU64, Ordering};

use adw::prelude::*;
use gtk4::{Box as GBox, Button, Entry, Image, Label, Orientation, ProgressBar, Spinner, Switch};
use libadwaita as adw;

use super::command_review::{
    set_review_feedback, CommandReviewCard, CommandReviewSpec, ReviewPresentation,
};
use super::UiState;
use crate::agent::{AgentSession, AgentState, ModelOutcome, ProposalId, ProposalStatus, Turn};
use crate::block_view::TermView;

const MAX_AGENT_MESSAGE_DISPLAY_BYTES: usize = 64 * 1024;
const MAX_AGENT_STATUS_DISPLAY_BYTES: usize = 16 * 1024;
const MAX_AGENT_INPUT_BYTES: usize = 16 * 1024;
const MAX_AGENT_ACTIVITY_DISPLAY_BYTES: usize = 1024 * 1024;
const MAX_AGENT_ACTIVITY_CARDS: usize = 128;

/// Window-lifetime ownership for Agent UI resources that must survive an
/// individual `AgentRuntime`. Reopening the panel or starting a new task must
/// not reset either the visible-history budget or TermView event bridges.
#[derive(Default)]
pub(crate) struct AgentUiLifetime {
    activity: RefCell<AgentActivityHistory>,
    bridged_targets: RefCell<Vec<Weak<TermView>>>,
}

#[derive(Default)]
struct AgentActivityHistory {
    budget: AgentActivityBudget,
    entries: VecDeque<AgentActivityEntry>,
}

struct AgentActivityEntry {
    target: Weak<TermView>,
    widget: gtk4::Widget,
}

/// Toolkit-free mirror of the visible activity FIFO. Keeping the accounting
/// independent makes the two hard bounds straightforward to test without a
/// display server.
#[derive(Debug, Default, PartialEq, Eq)]
struct AgentActivityBudget {
    item_bytes: VecDeque<usize>,
    total_display_bytes: usize,
}

impl AgentActivityBudget {
    /// Record a newest activity card and return how many oldest cards must be
    /// removed from the GTK block flows to satisfy both hard limits.
    fn push(&mut self, display_bytes: usize) -> usize {
        self.item_bytes.push_back(display_bytes);
        self.total_display_bytes = self.total_display_bytes.saturating_add(display_bytes);

        let mut evicted = 0;
        while self.item_bytes.len() > MAX_AGENT_ACTIVITY_CARDS
            || self.total_display_bytes > MAX_AGENT_ACTIVITY_DISPLAY_BYTES
        {
            let Some(oldest) = self.item_bytes.pop_front() else {
                self.total_display_bytes = 0;
                break;
            };
            self.total_display_bytes = self.total_display_bytes.saturating_sub(oldest);
            evicted += 1;
        }
        if self.item_bytes.is_empty() {
            // Also recovers deterministically from a theoretical usize
            // saturation in the addition above.
            self.total_display_bytes = 0;
        }
        evicted
    }
}

impl AgentUiLifetime {
    fn insert_activity(&self, target: &Rc<TermView>, widget: &gtk4::Widget, display_bytes: usize) {
        target.insert_inline_notice(widget);

        let evicted = {
            let mut history = self.activity.borrow_mut();
            history.entries.push_back(AgentActivityEntry {
                target: Rc::downgrade(target),
                widget: widget.clone(),
            });
            let evicted = history.budget.push(display_bytes);
            debug_assert!(evicted <= history.entries.len());
            (0..evicted)
                .filter_map(|_| history.entries.pop_front())
                .collect::<Vec<_>>()
        };

        // GTK mutation stays outside the RefCell borrow. The oldest visible
        // Agent cards are removed globally across every pane in this window.
        for entry in evicted {
            if let Some(target) = entry.target.upgrade() {
                target.remove_inline_notice(&entry.widget);
            }
        }
    }

    /// Claim the one permanent event bridge for this concrete TermView.
    fn claim_event_bridge(&self, target: &Rc<TermView>) -> bool {
        let mut targets = self.bridged_targets.borrow_mut();
        claim_weak_target(&mut targets, target)
    }
}

fn claim_weak_target<T>(targets: &mut Vec<Weak<T>>, target: &Rc<T>) -> bool {
    targets.retain(|candidate| candidate.upgrade().is_some());
    if targets
        .iter()
        .filter_map(Weak::upgrade)
        .any(|candidate| Rc::ptr_eq(&candidate, target))
    {
        return false;
    }
    targets.push(Rc::downgrade(target));
    true
}

/// Correlate one finished block with the approval that armed it.
///
/// A completion must first carry the exact locally armed generation. A stale,
/// manual, or unrelated completion leaves the current approval untouched even
/// if its command text is identical. Once the generation matches, the pending
/// entry is one-shot and consumed whether or not the secondary command-text
/// check succeeds, so no later completion can inherit that approval.
#[derive(Debug, PartialEq, Eq)]
enum PendingCompletion<T> {
    Unrelated,
    Matched(T),
    CommandMismatch,
}

fn take_pending_for_finished_block<T>(
    pending: &mut Option<PendingExecution<T>>,
    captured_command: &str,
    completed_generation: Option<u64>,
) -> PendingCompletion<T> {
    let Some(expected_generation) = pending.as_ref().map(|pending| pending.generation) else {
        return PendingCompletion::Unrelated;
    };
    if completed_generation != Some(expected_generation) {
        log::debug!("ignored Block completion that did not carry the pending Agent generation");
        return PendingCompletion::Unrelated;
    }
    let PendingExecution {
        value,
        command: approved_command,
        generation: _,
    } = pending
        .take()
        .expect("a matching generation has a pending Agent execution");
    if captured_command != approved_command {
        // Do not log either command: command text can contain sensitive data.
        log::debug!("Agent block completed with a differing VTE command capture; not observed");
        return PendingCompletion::CommandMismatch;
    }
    PendingCompletion::Matched(value)
}

fn resolve_pending_for_finished_block<T>(
    session: &mut AgentSession,
    pending: &mut Option<PendingExecution<T>>,
    captured_command: &str,
    completed_generation: Option<u64>,
) -> PendingCompletion<T> {
    let completion =
        take_pending_for_finished_block(pending, captured_command, completed_generation);
    if matches!(&completion, PendingCompletion::CommandMismatch) {
        session.cancel();
    }
    completion
}

fn take_pending_for_lost_execution<T>(
    pending: &mut Option<PendingExecution<T>>,
    generation: u64,
) -> Option<T> {
    if pending.as_ref().map(|pending| pending.generation) != Some(generation) {
        return None;
    }
    pending.take().map(|pending| pending.value)
}

/// One armed execution: the approval it belongs to, the exact command that was
/// submitted, and a checked, never-reused local identity.
#[derive(Debug, Clone, PartialEq, Eq)]
struct PendingExecution<T> {
    value: T,
    command: String,
    generation: u64,
}

fn proposal_callback_is_current(
    alive: bool,
    current_epoch: u64,
    captured_epoch: u64,
    state: AgentState,
    proposal_id: ProposalId,
) -> bool {
    alive
        && current_epoch == captured_epoch
        && state == AgentState::AwaitingApproval { proposal_id }
}

/// The one live Shell Agent session, stored in `UiState::agent_session`.
/// Closing it cancels the session and removes its inline card.
pub(crate) struct AgentHandle {
    runtime: Rc<AgentRuntime>,
}

impl AgentHandle {
    pub(crate) fn shutdown(&self) {
        self.runtime.shutdown();
    }

    /// Persist the live session for the next run (or clear a stale snapshot
    /// when there is nothing to save). Called on window close, before
    /// shutdown cancels the session.
    pub(crate) fn persist(&self) {
        let path = agent_snapshot_path();
        if let Some(snapshot) = self.runtime.session.borrow().snapshot() {
            if let Err(error) = write_agent_snapshot_file(&path, &snapshot) {
                log::warn!("agent: could not persist session: {error}");
            }
        }
        // This process never removes the shared public path after a restore:
        // loading consumes its own predecessor through a private claim. A
        // delete here could erase a checkpoint written by another process.
    }
}

fn agent_snapshot_path() -> std::path::PathBuf {
    let mut path = crate::config::config_file_path();
    path.set_file_name("agent_session.json");
    path
}

#[cfg(unix)]
static AGENT_SNAPSHOT_CLAIM_COUNTER: AtomicU64 = AtomicU64::new(0);

/// A snapshot is renamed away from its public name before it is decoded.  The
/// retained directory and file descriptors make the claim both single-winner
/// across processes and immune to a final-component namespace swap while it
/// is being consumed.
#[cfg(unix)]
struct AgentSnapshotClaim {
    directory: std::fs::File,
    file: std::fs::File,
    parent: std::path::PathBuf,
    original_name: std::ffi::OsString,
    claimed_name: std::ffi::OsString,
}

#[cfg(unix)]
impl AgentSnapshotClaim {
    fn open_relative(
        directory: &std::fs::File,
        name: &std::ffi::OsStr,
    ) -> std::io::Result<std::fs::File> {
        use std::os::fd::{AsRawFd, FromRawFd};
        use std::os::unix::ffi::OsStrExt;

        let name = std::ffi::CString::new(name.as_bytes()).map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "agent snapshot name contains NUL",
            )
        })?;
        // SAFETY: `name` is NUL terminated, the retained directory descriptor
        // is live, and ownership of a successful descriptor is transferred to
        // `File` exactly once.
        let descriptor = unsafe {
            nix::libc::openat(
                directory.as_raw_fd(),
                name.as_ptr(),
                nix::libc::O_RDONLY
                    | nix::libc::O_NOFOLLOW
                    | nix::libc::O_NONBLOCK
                    | nix::libc::O_CLOEXEC,
            )
        };
        if descriptor < 0 {
            Err(std::io::Error::last_os_error())
        } else {
            // SAFETY: `descriptor` is newly returned and uniquely owned.
            Ok(unsafe { std::fs::File::from_raw_fd(descriptor) })
        }
    }

    fn validate_file(file: &std::fs::File) -> std::io::Result<std::fs::Metadata> {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};

        let metadata = file.metadata()?;
        // SAFETY: geteuid has no preconditions and only reads process state.
        if !metadata.is_file()
            || metadata.uid() != unsafe { nix::libc::geteuid() }
            || metadata.nlink() != 1
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "agent snapshot must be a current-user regular file with one hard link",
            ));
        }
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "agent snapshot must not be accessible by group or other users",
            ));
        }
        Ok(metadata)
    }

    fn entry_still_matches(&self) -> std::io::Result<bool> {
        use std::os::unix::fs::MetadataExt;

        let current = Self::open_relative(&self.directory, &self.claimed_name)?;
        let expected = self.file.metadata()?;
        let current = current.metadata()?;
        Ok(expected.dev() == current.dev() && expected.ino() == current.ino())
    }

    fn read_snapshot(
        &self,
    ) -> Result<crate::agent::AgentSessionSnapshot, crate::agent::AgentSnapshotError> {
        use std::io::Read;

        let metadata = Self::validate_file(&self.file).map_err(|error| {
            crate::agent::AgentSnapshotError::Decode(format!(
                "inspect claimed agent snapshot: {error}"
            ))
        })?;
        let limit = crate::agent::MAX_AGENT_SNAPSHOT_JSON_BYTES as u64;
        if metadata.len() > limit {
            return Err(crate::agent::AgentSnapshotError::Decode(format!(
                "claimed agent snapshot exceeds {limit} bytes"
            )));
        }
        let mut reader = self.file.try_clone().map_err(|error| {
            crate::agent::AgentSnapshotError::Decode(format!(
                "clone claimed agent snapshot: {error}"
            ))
        })?;
        let mut bytes = Vec::with_capacity(metadata.len() as usize);
        reader
            .by_ref()
            .take(limit.saturating_add(1))
            .read_to_end(&mut bytes)
            .map_err(|error| {
                crate::agent::AgentSnapshotError::Decode(format!(
                    "read claimed agent snapshot: {error}"
                ))
            })?;
        if bytes.len() as u64 > limit {
            return Err(crate::agent::AgentSnapshotError::Decode(format!(
                "claimed agent snapshot exceeds {limit} bytes"
            )));
        }
        let encoded = String::from_utf8(bytes).map_err(|_| {
            crate::agent::AgentSnapshotError::Decode(
                "claimed agent snapshot is not valid UTF-8".to_string(),
            )
        })?;
        crate::agent::AgentSessionSnapshot::from_json(&encoded)
    }

    fn retire(self) -> std::io::Result<()> {
        use std::os::fd::AsRawFd;
        use std::os::unix::ffi::OsStrExt;

        if !self.entry_still_matches()? {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "claimed agent snapshot entry changed before retirement",
            ));
        }
        let name = std::ffi::CString::new(self.claimed_name.as_bytes()).map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "agent snapshot name contains NUL",
            )
        })?;
        // SAFETY: the name is valid for the retained directory descriptor and
        // unlinkat retains no pointer after returning.
        if unsafe { nix::libc::unlinkat(self.directory.as_raw_fd(), name.as_ptr(), 0) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        self.directory.sync_all()
    }

    fn quarantine(self) -> std::io::Result<std::path::PathBuf> {
        use std::os::fd::AsRawFd;
        use std::os::unix::ffi::{OsStrExt, OsStringExt};

        if !self.entry_still_matches()? {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "claimed agent snapshot entry changed before quarantine",
            ));
        }
        let source = std::ffi::CString::new(self.claimed_name.as_bytes()).map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "agent snapshot name contains NUL",
            )
        })?;
        for _ in 0..16 {
            let nonce = AGENT_SNAPSHOT_CLAIM_COUNTER.fetch_add(1, Ordering::Relaxed);
            let mut target = self.original_name.clone().into_vec();
            target.extend_from_slice(format!(".corrupt-{}-{nonce}", std::process::id()).as_bytes());
            let target = std::ffi::OsString::from_vec(target);
            let target_c = std::ffi::CString::new(target.as_bytes()).map_err(|_| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "agent quarantine name contains NUL",
                )
            })?;
            #[cfg(target_os = "linux")]
            // SAFETY: both names and the retained descriptor are live for the
            // duration of the call; renameat2 retains no pointers.
            let result = unsafe {
                nix::libc::renameat2(
                    self.directory.as_raw_fd(),
                    source.as_ptr(),
                    self.directory.as_raw_fd(),
                    target_c.as_ptr(),
                    nix::libc::RENAME_NOREPLACE,
                )
            };
            #[cfg(not(target_os = "linux"))]
            let result = unsafe {
                nix::libc::renameat(
                    self.directory.as_raw_fd(),
                    source.as_ptr(),
                    self.directory.as_raw_fd(),
                    target_c.as_ptr(),
                )
            };
            if result == 0 {
                self.directory.sync_all()?;
                return Ok(self.parent.join(target));
            }
            let error = std::io::Error::last_os_error();
            if error.kind() != std::io::ErrorKind::AlreadyExists {
                return Err(error);
            }
        }
        Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "could not allocate an agent snapshot quarantine name",
        ))
    }
}

#[cfg(unix)]
fn claim_agent_snapshot_file(
    path: &std::path::Path,
) -> std::io::Result<Option<AgentSnapshotClaim>> {
    use std::os::fd::AsRawFd;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};

    let original_name = path
        .file_name()
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "agent snapshot path has no file name",
            )
        })?
        .to_os_string();
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| std::path::Path::new("."));
    let directory = match std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_DIRECTORY | nix::libc::O_NOFOLLOW | nix::libc::O_CLOEXEC)
        .open(parent)
    {
        Ok(directory) => directory,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let directory_metadata = directory.metadata()?;
    // SAFETY: geteuid has no preconditions and only reads process state.
    if !directory_metadata.is_dir()
        || directory_metadata.uid() != unsafe { nix::libc::geteuid() }
        || directory_metadata.permissions().mode() & 0o022 != 0
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "agent snapshot parent must be current-user owned and not group/world writable",
        ));
    }
    let source = match AgentSnapshotClaim::open_relative(&directory, &original_name) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    // Validate the source descriptor before moving the entry. A second
    // descriptor opened after the rename is compared below, closing the
    // remaining check/rename race without following links or blocking on a
    // FIFO.
    AgentSnapshotClaim::validate_file(&source)?;
    let expected = source.metadata()?;
    let source_c = std::ffi::CString::new(original_name.as_bytes()).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "agent snapshot name contains NUL",
        )
    })?;

    for _ in 0..16 {
        let nonce = AGENT_SNAPSHOT_CLAIM_COUNTER.fetch_add(1, Ordering::Relaxed);
        let mut claimed_name = std::ffi::OsString::from(".");
        claimed_name.push(&original_name);
        claimed_name.push(format!(".claim-{}-{nonce}", std::process::id()));
        let claimed_c = std::ffi::CString::new(claimed_name.as_bytes()).map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "agent snapshot claim name contains NUL",
            )
        })?;
        #[cfg(target_os = "linux")]
        // SAFETY: names and descriptor remain live throughout the call and no
        // pointers are retained.
        let result = unsafe {
            nix::libc::renameat2(
                directory.as_raw_fd(),
                source_c.as_ptr(),
                directory.as_raw_fd(),
                claimed_c.as_ptr(),
                nix::libc::RENAME_NOREPLACE,
            )
        };
        #[cfg(not(target_os = "linux"))]
        let result = unsafe {
            nix::libc::renameat(
                directory.as_raw_fd(),
                source_c.as_ptr(),
                directory.as_raw_fd(),
                claimed_c.as_ptr(),
            )
        };
        if result != 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::NotFound {
                return Ok(None);
            }
            if error.kind() == std::io::ErrorKind::AlreadyExists {
                continue;
            }
            return Err(error);
        }
        directory.sync_all()?;
        let claimed = AgentSnapshotClaim::open_relative(&directory, &claimed_name)?;
        let actual = AgentSnapshotClaim::validate_file(&claimed)?;
        if expected.dev() != actual.dev() || expected.ino() != actual.ino() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "agent snapshot entry changed while it was being claimed",
            ));
        }
        return Ok(Some(AgentSnapshotClaim {
            directory,
            file: claimed,
            parent: parent.to_path_buf(),
            original_name,
            claimed_name,
        }));
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "could not allocate an agent snapshot claim name",
    ))
}

fn write_agent_snapshot_file(
    path: &std::path::Path,
    snapshot: &crate::agent::AgentSessionSnapshot,
) -> Result<(), crate::agent::AgentSnapshotError> {
    let encoded = snapshot.to_json()?;
    crate::config_store::write_private_bytes(
        path,
        encoded.as_bytes(),
        crate::agent::MAX_AGENT_SNAPSHOT_JSON_BYTES,
    )
    .map_err(|error| {
        crate::agent::AgentSnapshotError::Encode(format!("write {}: {error}", path.display()))
    })
}

fn read_agent_snapshot_file(
    path: &std::path::Path,
) -> Result<Option<crate::agent::AgentSessionSnapshot>, crate::agent::AgentSnapshotError> {
    let bytes = crate::config_store::read_private_bytes(
        path,
        crate::agent::MAX_AGENT_SNAPSHOT_JSON_BYTES as u64,
    )
    .map_err(|error| {
        crate::agent::AgentSnapshotError::Decode(format!("read {}: {error}", path.display()))
    })?;
    let Some(bytes) = bytes else {
        return Ok(None);
    };
    let encoded = String::from_utf8(bytes).map_err(|_| {
        crate::agent::AgentSnapshotError::Decode(format!("{} is not valid UTF-8", path.display()))
    })?;
    crate::agent::AgentSessionSnapshot::from_json(&encoded).map(Some)
}

/// Validate proposal identity and lifecycle on jagent's bounded decoded
/// transcript, before `AgentSession::restore` compacts it. Validating only the
/// restored session is insufficient: prefix compaction can hide a
/// duplicate/older proposal that an approval click could otherwise rebind to.
fn audit_agent_snapshot(
    snapshot: &crate::agent::AgentSessionSnapshot,
) -> Result<(), crate::agent::AgentSnapshotError> {
    const MAX_RESTORED_TURNS: usize = 128;
    const PROPOSAL_ID_HEADROOM: u64 = 1_024;

    // jagent's sole snapshot wire decoder has already enforced allocation
    // budgets. Keep Forge's stricter semantic audit on that unmodified view.
    let transcript = snapshot.transcript();
    let transcript_truncated = snapshot.transcript_truncated();
    let state = snapshot.state();
    let turns_used = snapshot.turns_used();
    let max_turns = snapshot.max_turns();
    let next_proposal_id = snapshot.next_proposal_id();
    if snapshot.version() != 1
        || max_turns == 0
        || max_turns > 1_000
        || turns_used > max_turns
        || transcript.is_empty()
        || transcript.len() > MAX_RESTORED_TURNS
    {
        return Err(crate::agent::AgentSnapshotError::Invalid(
            "snapshot scalar or transcript limit is invalid",
        ));
    }
    if next_proposal_id == 0 || next_proposal_id > u64::MAX - PROPOSAL_ID_HEADROOM {
        return Err(crate::agent::AgentSnapshotError::Invalid(
            "proposal identifier counter is invalid or nearly exhausted",
        ));
    }

    let mut previous_proposal_id = None;
    let mut proposal_ids = HashMap::new();
    let mut pending = Vec::new();
    let mut observations = HashMap::new();
    let mut model_actions = 0_u32;
    let mut protocol_errors = 0_u32;
    for (index, turn) in transcript.iter().enumerate() {
        match turn {
            Turn::AssistantProposed { id, status, .. } => {
                model_actions = model_actions.saturating_add(1);
                let value = id.get();
                if value == 0 || value >= next_proposal_id {
                    return Err(crate::agent::AgentSnapshotError::Invalid(
                        "proposal id is zero or not below the next-id counter",
                    ));
                }
                if let Some(previous) = previous_proposal_id {
                    if value != previous + 1 {
                        return Err(crate::agent::AgentSnapshotError::Invalid(
                            "proposal ids are duplicated, reordered, or non-contiguous",
                        ));
                    }
                } else if !transcript_truncated && value != 1 {
                    return Err(crate::agent::AgentSnapshotError::Invalid(
                        "untruncated proposal ids do not start at one",
                    ));
                }
                previous_proposal_id = Some(value);
                if proposal_ids.insert(*id, (*status, index)).is_some() {
                    return Err(crate::agent::AgentSnapshotError::Invalid(
                        "duplicate proposal id",
                    ));
                }
                if *status == ProposalStatus::Pending {
                    pending.push((*id, index));
                }
            }
            Turn::Observation { proposal_id, .. } => {
                let approved_immediately_before =
                    proposal_ids
                        .get(proposal_id)
                        .is_some_and(|(status, proposal_index)| {
                            *status == ProposalStatus::Approved && *proposal_index + 1 == index
                        });
                if observations.insert(*proposal_id, index).is_some()
                    || !approved_immediately_before
                {
                    return Err(crate::agent::AgentSnapshotError::Invalid(
                        "observation is duplicate or does not immediately follow its approved proposal",
                    ));
                }
            }
            Turn::AssistantSay(_) => model_actions = model_actions.saturating_add(1),
            Turn::ProtocolError(_) => protocol_errors = protocol_errors.saturating_add(1),
            _ => {}
        }
    }

    // Every retained proposal/say consumed one model turn. A ProtocolError may
    // either be a parse failure (one turn) or a transport failure (no turn),
    // which gives an exact range while the transcript is untruncated.
    if turns_used < model_actions
        || (!transcript_truncated && turns_used > model_actions.saturating_add(protocol_errors))
    {
        return Err(crate::agent::AgentSnapshotError::Invalid(
            "turn counter is inconsistent with the transcript",
        ));
    }

    match previous_proposal_id {
        Some(last) if next_proposal_id != last + 1 => {
            return Err(crate::agent::AgentSnapshotError::Invalid(
                "next proposal id does not immediately follow the transcript",
            ));
        }
        None if !transcript_truncated && next_proposal_id != 1 => {
            return Err(crate::agent::AgentSnapshotError::Invalid(
                "proposal id counter was reset or advanced without a proposal",
            ));
        }
        _ => {}
    }

    let final_index = transcript.len() - 1;
    let final_turn = &transcript[final_index];
    match state {
        AgentState::AwaitingApproval { proposal_id }
            if pending.as_slice() == [(proposal_id, final_index)] => {}
        AgentState::AwaitingApproval { .. } => {
            return Err(crate::agent::AgentSnapshotError::Invalid(
                "approval state does not point to the sole final pending proposal",
            ));
        }
        AgentState::AwaitingObservation { proposal_id } => {
            if !pending.is_empty()
                || observations.contains_key(&proposal_id)
                || proposal_ids.get(&proposal_id) != Some(&(ProposalStatus::Approved, final_index))
            {
                return Err(crate::agent::AgentSnapshotError::Invalid(
                    "observation state does not point to the final approved proposal",
                ));
            }
        }
        _ if !pending.is_empty() => {
            return Err(crate::agent::AgentSnapshotError::Invalid(
                "pending proposal exists outside approval state",
            ));
        }
        _ => {}
    }
    let final_state_is_valid = match state {
        AgentState::Ready => {
            turns_used < max_turns
                && matches!(
                    final_turn,
                    Turn::AssistantSay(_)
                        | Turn::ProtocolError(_)
                        | Turn::AssistantProposed {
                            status: ProposalStatus::ManualReview,
                            ..
                        }
                )
        }
        AgentState::AwaitingModel => {
            turns_used < max_turns
                && matches!(
                    final_turn,
                    Turn::User(_)
                        | Turn::ProtocolError(_)
                        | Turn::Observation { .. }
                        | Turn::AssistantProposed {
                            status: ProposalStatus::Rejected,
                            ..
                        }
                )
        }
        AgentState::AwaitingApproval { .. } => true,
        AgentState::AwaitingObservation { .. } => true,
        AgentState::Completed => matches!(final_turn, Turn::AssistantSay(_)),
        AgentState::TurnLimitReached => {
            turns_used == max_turns
                && matches!(
                    final_turn,
                    Turn::AssistantSay(_)
                        | Turn::ProtocolError(_)
                        | Turn::Observation { .. }
                        | Turn::AssistantProposed {
                            status: ProposalStatus::Rejected | ProposalStatus::ManualReview,
                            ..
                        }
                )
        }
        AgentState::Cancelled => false,
    };
    if !final_state_is_valid {
        return Err(crate::agent::AgentSnapshotError::Invalid(
            "session state does not match the final transcript turn or budget",
        ));
    }
    for (proposal_id, (status, _)) in &proposal_ids {
        if *status != ProposalStatus::Approved {
            continue;
        }
        let is_current_unobserved = matches!(
            state,
            AgentState::AwaitingObservation {
                proposal_id: current
            } if current == *proposal_id
        );
        if observations.contains_key(proposal_id) == is_current_unobserved {
            return Err(crate::agent::AgentSnapshotError::Invalid(
                "approved proposal observation lifecycle is inconsistent",
            ));
        }
    }
    Ok(())
}

/// Apply Forge's stricter transcript-ID and proposal-lifecycle contract at the
/// app boundary so a crafted snapshot cannot make a visible approval card
/// authorize a different transcript entry (confused deputy).
fn validate_agent_snapshot(
    snapshot: crate::agent::AgentSessionSnapshot,
) -> Result<AgentSession, crate::agent::AgentSnapshotError> {
    const MAX_RESTORED_TURNS: usize = 128;
    const MAX_RESTORED_MESSAGE_BYTES: usize = 16 * 1024;
    const MAX_RESTORED_THOUGHT_BYTES: usize = 4 * 1024;
    const MAX_RESTORED_COMMAND_BYTES: usize = 16 * 1024;
    const MAX_RESTORED_OBSERVATION_BYTES: usize = 4 * 1024;
    const PROPOSAL_ID_HEADROOM: u64 = 1_024;

    // Keep enough identifier space for every possible remaining turn so a
    // crafted near-max counter cannot exhaust the authorization namespace.
    audit_agent_snapshot(&snapshot)?;

    let session = AgentSession::restore(snapshot)?;
    if session.transcript().len() > MAX_RESTORED_TURNS {
        return Err(crate::agent::AgentSnapshotError::Invalid(
            "transcript has too many turns",
        ));
    }
    let mut proposal_ids = HashMap::new();
    let mut pending = Vec::new();
    let mut observation_ids = HashSet::new();
    for turn in session.transcript() {
        match turn {
            Turn::User(message) | Turn::AssistantSay(message) => {
                if message.len() > MAX_RESTORED_MESSAGE_BYTES {
                    return Err(crate::agent::AgentSnapshotError::Invalid(
                        "transcript message exceeds its byte limit",
                    ));
                }
            }
            Turn::AssistantThought(thought) => {
                if thought.len() > MAX_RESTORED_THOUGHT_BYTES {
                    return Err(crate::agent::AgentSnapshotError::Invalid(
                        "transcript thought exceeds its byte limit",
                    ));
                }
            }
            Turn::AssistantProposed {
                id,
                command,
                status,
            } => {
                if id.get() > u64::MAX - PROPOSAL_ID_HEADROOM {
                    return Err(crate::agent::AgentSnapshotError::Invalid(
                        "proposal identifier space is nearly exhausted",
                    ));
                }
                if proposal_ids.insert(*id, *status).is_some() {
                    return Err(crate::agent::AgentSnapshotError::Invalid(
                        "duplicate proposal id",
                    ));
                }
                if command.is_empty()
                    || command.len() > MAX_RESTORED_COMMAND_BYTES
                    || command.chars().any(char::is_control)
                    || crate::review_input::contains_visual_spoof(command)
                {
                    return Err(crate::agent::AgentSnapshotError::Invalid(
                        "proposal command is invalid",
                    ));
                }
                if *status == ProposalStatus::Pending {
                    pending.push(*id);
                }
            }
            Turn::Observation {
                proposal_id,
                output_sample,
                ..
            } => {
                if output_sample.len() > MAX_RESTORED_OBSERVATION_BYTES
                    || !observation_ids.insert(*proposal_id)
                    || proposal_ids.get(proposal_id) != Some(&ProposalStatus::Approved)
                {
                    return Err(crate::agent::AgentSnapshotError::Invalid(
                        "observation is duplicated, oversized, out of order, or not approved",
                    ));
                }
            }
            Turn::ProtocolError(message) => {
                if message.len() > MAX_RESTORED_MESSAGE_BYTES {
                    return Err(crate::agent::AgentSnapshotError::Invalid(
                        "protocol error exceeds its byte limit",
                    ));
                }
            }
        }
    }
    match session.state() {
        AgentState::AwaitingApproval { proposal_id } if pending.as_slice() == [proposal_id] => {}
        AgentState::AwaitingApproval { .. } => {
            return Err(crate::agent::AgentSnapshotError::Invalid(
                "approval state does not identify exactly one pending proposal",
            ));
        }
        _ if !pending.is_empty() => {
            return Err(crate::agent::AgentSnapshotError::Invalid(
                "pending proposal exists outside approval state",
            ));
        }
        _ => {}
    }
    Ok(session)
}

/// Restore a snapshot for a process-local rollback. Unlike the persisted-file
/// loader below, this must reject an execution checkpoint whose one-shot PTY
/// generation belongs to a different process lifetime.
fn restore_agent_snapshot(
    snapshot: crate::agent::AgentSessionSnapshot,
) -> Result<AgentSession, crate::agent::AgentSnapshotError> {
    let awaiting_observation = matches!(snapshot.state(), AgentState::AwaitingObservation { .. });
    let session = validate_agent_snapshot(snapshot)?;
    if awaiting_observation {
        return Err(crate::agent::AgentSnapshotError::Invalid(
            "an approved command awaiting observation cannot be rebound after restart",
        ));
    }
    Ok(session)
}

enum AgentSnapshotLoad {
    Restored(AgentSession),
    /// The snapshot is internally valid, but its approved execution was tied
    /// to a process-local PTY generation that cannot be observed after restart.
    RetireUnresumable,
}

fn validate_agent_snapshot_for_load(
    snapshot: crate::agent::AgentSessionSnapshot,
) -> Result<AgentSnapshotLoad, crate::agent::AgentSnapshotError> {
    let awaiting_observation = matches!(snapshot.state(), AgentState::AwaitingObservation { .. });
    let session = validate_agent_snapshot(snapshot)?;
    Ok(if awaiting_observation {
        AgentSnapshotLoad::RetireUnresumable
    } else {
        AgentSnapshotLoad::Restored(session)
    })
}

#[cfg(unix)]
fn load_agent_snapshot(path: &std::path::Path) -> Option<AgentSession> {
    let claim = match claim_agent_snapshot_file(path) {
        Ok(Some(claim)) => claim,
        Ok(None) => return None,
        Err(error) => {
            log::warn!(
                "agent: could not exclusively claim saved session {}: {error}",
                path.display()
            );
            return None;
        }
    };
    match claim
        .read_snapshot()
        .and_then(validate_agent_snapshot_for_load)
    {
        Ok(AgentSnapshotLoad::Restored(session)) => {
            // The public path disappeared at claim time. Retire and sync the
            // unique claimed entry before exposing the restored proposal to
            // the UI, so no second process can ever approve the same snapshot.
            if let Err(error) = claim.retire() {
                log::warn!("agent: could not retire claimed saved session: {error}");
                None
            } else {
                Some(session)
            }
        }
        Ok(AgentSnapshotLoad::RetireUnresumable) => {
            // This is not corrupt data. The command may or may not have run,
            // but its process-local completion identity is gone, so consume
            // the checkpoint exactly once and reopen with a fresh Ready
            // session without guessing an observation.
            if let Err(error) = claim.retire() {
                log::warn!("agent: could not retire unresumable saved session: {error}");
            }
            None
        }
        Err(error) => {
            log::warn!("agent: rejecting invalid saved session: {error}");
            if let Err(quarantine_error) = claim.quarantine() {
                log::warn!(
                    "agent: could not quarantine invalid claimed snapshot: {quarantine_error}"
                );
            }
            None
        }
    }
}

#[cfg(not(unix))]
fn load_agent_snapshot(path: &std::path::Path) -> Option<AgentSession> {
    log::warn!(
        "agent: saved-session restore is disabled without atomic Unix file claims ({})",
        path.display()
    );
    None
}

struct AgentRuntime {
    session: RefCell<AgentSession>,
    target: Rc<TermView>,
    ui_lifetime: Rc<AgentUiLifetime>,
    config: Rc<RefCell<crate::config::Config>>,
    shell: String,
    block_context: RefCell<Option<crate::ai::BlockContext>>,
    /// The inline card widget inserted into the target pane's block list.
    card: gtk4::Widget,
    input: Entry,
    send: Button,
    stop_request: Button,
    retry_request: Button,
    context_clear: Button,
    context_attach: Button,
    context_card: GBox,
    context_label: Label,
    status: Label,
    status_spinner: Spinner,
    prompt_status: Label,
    turn_progress: ProgressBar,
    turn_label: Label,
    session_action: Button,
    proposal_box: GBox,
    /// jagent keeps ProposalId monotonic across fresh tasks. Every UI callback
    /// also carries this app-owned epoch as defense in depth across session
    /// replacement, preventing a delayed old card/dialog from authorizing a
    /// proposal in a newer task.
    task_epoch: Cell<u64>,
    pending_command: RefCell<Option<PendingExecution<ProposalId>>>,
    /// Checked, never-reused execution identity. Wrapping it would let a late
    /// completion from an earlier execution look like the current one.
    next_execution_generation: Cell<u64>,
    request_cancellation: RefCell<Option<crate::ai::AiCancellationToken>>,
    busy: Cell<bool>,
    alive: Cell<bool>,
    /// Content-free lifecycle phases for the ASCII organism: only coarse
    /// state kinds cross, never proposals, commands, or model output.
    organism_agent: Rc<super::OrganismAgentSignal>,
}

impl AgentRuntime {
    /// Add one conversation message as its own block in the pane's block flow,
    /// directly above the pinned Agent card. Messages are ordinary blocks in
    /// the conversation: they stay in place as history and survive the session
    /// card being closed.
    fn append(&self, speaker: &str, body: &str) {
        let compact = self.config.borrow().block_compact;
        let message = build_agent_message_block(speaker, body, compact);
        self.ui_lifetime.insert_activity(
            &self.target,
            &message,
            agent_message_display_bytes(speaker, body),
        );
        // Keep the session card below the newest message, pinned above the
        // live prompt. (On the intro message this also performs the card's
        // initial insertion.)
        self.target.insert_inline_notice(&self.card);
    }

    fn clear_proposal(&self) {
        while let Some(child) = self.proposal_box.first_child() {
            self.proposal_box.remove(&child);
        }
        self.proposal_box.set_visible(false);
    }

    fn proposal_callback_is_current(&self, epoch: u64, proposal_id: ProposalId) -> bool {
        proposal_callback_is_current(
            self.alive.get(),
            self.task_epoch.get(),
            epoch,
            self.session.borrow().state(),
            proposal_id,
        )
    }

    fn set_status(&self, message: &str, active: bool) {
        self.status
            .set_text(&crate::review_input::safe_inline_display(
                message,
                MAX_AGENT_STATUS_DISPLAY_BYTES,
            ));
        if active {
            self.status_spinner.start();
        } else {
            self.status_spinner.stop();
        }
        let session = self.session.borrow();
        let used = session.turns_used();
        let max = session.max_turns();
        self.turn_label.set_text(&format!("{used} / {max} turns"));
        self.turn_progress
            .set_fraction(f64::from(used) / f64::from(max.max(1)));
    }

    fn sync_controls(&self) {
        let session = self.session.borrow();
        let ready = self.alive.get() && !self.busy.get() && session.state() == AgentState::Ready;
        self.input.set_sensitive(ready);
        self.send
            .set_sensitive(ready && !self.input.text().trim().is_empty());
        self.stop_request
            .set_visible(self.alive.get() && self.busy.get());
        self.stop_request
            .set_sensitive(self.alive.get() && self.busy.get());
        self.retry_request
            .set_visible(self.alive.get() && !self.busy.get() && session.can_retry_model());
        self.retry_request
            .set_sensitive(self.retry_request.is_visible());
        let context_editable =
            self.alive.get() && !self.busy.get() && session.state() == AgentState::Ready;
        self.context_clear.set_sensitive(context_editable);
        self.context_attach.set_sensitive(context_editable);
        let can_follow_up =
            self.alive.get() && !self.busy.get() && session.can_continue_after_completion();
        let can_start_new = self.alive.get()
            && !self.busy.get()
            && matches!(
                session.state(),
                AgentState::Completed | AgentState::TurnLimitReached
            )
            && !can_follow_up;
        self.session_action
            .set_visible(can_follow_up || can_start_new);
        self.session_action
            .set_sensitive(can_follow_up || can_start_new);
        if can_follow_up {
            self.session_action.set_label("Follow up");
            self.session_action.set_tooltip_text(Some(
                "Continue with the completed task and keep its Agent context",
            ));
        } else if can_start_new {
            self.session_action.set_label("New task");
            self.session_action.set_tooltip_text(Some(
                "Start a fresh Agent task in this pane with a reset turn budget",
            ));
        }
        drop(session);
        self.sync_prompt_status();
    }

    fn sync_prompt_status(&self) {
        let prompt_status = self.target.agent_command_prompt_status();
        self.prompt_status.set_text(prompt_status.short_label());
        self.prompt_status
            .set_tooltip_text(Some(prompt_status.blocked_message()));
        self.prompt_status.remove_css_class("agent-prompt-ready");
        self.prompt_status.remove_css_class("agent-prompt-blocked");
        if prompt_status.is_ready() {
            self.prompt_status.add_css_class("agent-prompt-ready");
        } else {
            self.prompt_status.add_css_class("agent-prompt-blocked");
        }
    }

    fn resume_or_start_new(runtime: Rc<Self>) {
        if runtime.busy.get() || !runtime.alive.get() {
            return;
        }
        let starts_new = !runtime.session.borrow().can_continue_after_completion();
        if starts_new && runtime.task_epoch.get() == u64::MAX {
            runtime.render_session_state(Some(
                "Cannot start another Agent task because its safety epoch is exhausted.",
            ));
            return;
        }
        let result = {
            let mut session = runtime.session.borrow_mut();
            if session.can_continue_after_completion() {
                session.continue_after_completion().map(|()| false)
            } else {
                session.start_new_task().map(|()| true)
            }
        };
        match result {
            Ok(started_new) => {
                if started_new {
                    runtime.task_epoch.set(runtime.task_epoch.get() + 1);
                    runtime.clear_proposal();
                    runtime.pending_command.borrow_mut().take();
                    runtime.input.set_text("");
                    runtime.append(
                        "Agent",
                        "Started a fresh task in this pane. Previous activity remains visible but is no longer sent to the model.",
                    );
                    runtime.render_session_state(Some("Ready for a new task"));
                } else {
                    runtime.render_session_state(Some(
                        "Completed task reopened for a follow-up instruction",
                    ));
                }
            }
            Err(error) => runtime.render_session_state(Some(&error.to_string())),
        }
    }

    fn render_session_state(&self, ready_status: Option<&str>) {
        self.sync_controls();
        if self.busy.get() {
            self.organism_agent
                .note_phase(crate::organism::AgentPulse::Working);
            if let Some(message) = ready_status {
                self.set_status(message, true);
            }
            return;
        }
        let state = self.session.borrow().state();
        let pulse = match state {
            // An idle session awaiting instruction is not "working"; staying
            // silent here also keeps panel open/close toggles from pumping
            // alternating phases past the dedup.
            AgentState::Ready => None,
            AgentState::AwaitingModel | AgentState::AwaitingObservation { .. } => {
                Some(crate::organism::AgentPulse::Working)
            }
            AgentState::AwaitingApproval { .. } => Some(crate::organism::AgentPulse::AskingReview),
            AgentState::Completed | AgentState::TurnLimitReached => {
                Some(crate::organism::AgentPulse::Finished)
            }
            AgentState::Cancelled => Some(crate::organism::AgentPulse::Gone),
        };
        if let Some(pulse) = pulse {
            self.organism_agent.note_phase(pulse);
        }
        match state {
            AgentState::Ready => {
                self.set_status(
                    ready_status.unwrap_or("Ready for the next instruction"),
                    false,
                );
                self.input.grab_focus();
            }
            AgentState::AwaitingApproval { proposal_id } => self.set_status(
                &format!("Proposal #{} is waiting for review", proposal_id.get()),
                false,
            ),
            AgentState::AwaitingObservation { .. } => {
                self.set_status("Running the approved command…", true)
            }
            AgentState::Completed => self.set_status("Task completed", false),
            AgentState::Cancelled => self.set_status("Agent cancelled", false),
            AgentState::TurnLimitReached => self.set_status(
                "Turn limit reached. Start a new task to reset the Agent context and budget.",
                false,
            ),
            AgentState::AwaitingModel => {
                self.set_status(ready_status.unwrap_or("Waiting for the model…"), true)
            }
        }
    }

    fn stop_current_request(&self) {
        if !self.busy.get() || !self.alive.get() {
            return;
        }
        if let Some(cancellation) = self.request_cancellation.borrow().as_ref() {
            cancellation.cancel();
            self.stop_request.set_sensitive(false);
            self.set_status("Stopping the current model request…", true);
        }
    }

    fn retry_model(runtime: Rc<Self>) {
        if runtime.busy.get() || !runtime.alive.get() {
            return;
        }
        let retry_result = runtime.session.borrow_mut().retry_model();
        match retry_result {
            Ok(()) => Self::request_model(runtime),
            Err(error) => {
                runtime.render_session_state(Some(&error.to_string()));
            }
        }
    }

    fn detach_block_context(&self) {
        if self.busy.get() || self.session.borrow().state() != AgentState::Ready {
            return;
        }
        if self.block_context.borrow_mut().take().is_some() {
            self.context_card.set_visible(false);
            self.append(
                "Agent",
                "Selected Block context detached. Session activity is still retained.",
            );
            self.render_session_state(None);
        }
    }

    /// Attach (or replace) the currently selected finished Block as untrusted
    /// context mid-session, mirroring the capture that happens when the card
    /// opens.
    fn attach_block_context(&self) {
        if self.busy.get()
            || !self.alive.get()
            || self.session.borrow().state() != AgentState::Ready
        {
            return;
        }
        let Some(context) = self.target.selected_block_context(80) else {
            self.set_status(
                "Select a finished Block in this pane first, then attach it.",
                false,
            );
            return;
        };
        self.context_label
            .set_text(&agent_block_context_label(&context));
        self.context_label
            .set_tooltip_text(Some(&agent_block_context_tooltip(&context)));
        self.context_card.set_visible(true);
        let replaced = self.block_context.borrow_mut().replace(context).is_some();
        self.append(
            "Agent",
            if replaced {
                "Replaced the attached Block context with the currently selected finished Block."
            } else {
                "Attached the selected finished Block as untrusted context for upcoming instructions."
            },
        );
        self.render_session_state(None);
    }

    fn submit(runtime: Rc<Self>) {
        if runtime.busy.get() || !runtime.alive.get() {
            return;
        }
        let text = runtime.input.text().trim().to_string();
        if text.is_empty() {
            return;
        }
        let submit_result = runtime.session.borrow_mut().submit_user(text.clone());
        if let Err(error) = submit_result {
            runtime.render_session_state(Some(&error.to_string()));
            return;
        }
        runtime.input.set_text("");
        runtime.append("You", &text);
        Self::request_model(runtime);
    }

    fn request_model(runtime: Rc<Self>) {
        if !runtime.alive.get()
            || runtime.busy.get()
            || runtime.session.borrow().state() != AgentState::AwaitingModel
        {
            runtime.render_session_state(None);
            return;
        }

        let client = match crate::ai::client_from_config(&runtime.config.borrow()) {
            Ok(client) => client,
            Err(error) => {
                let message = error.to_string();
                let _ = runtime.session.borrow_mut().model_failed(&message);
                runtime.append("Error", &message);
                runtime.render_session_state(Some(&message));
                return;
            }
        };
        let cwd = runtime.target.cwd();
        // Bounded probe (short UI wait, then cached/stale): branch and dirty
        // state let the model tailor proposals to the repository.
        let git = crate::git_meta::read(std::path::Path::new(&cwd));
        let system = crate::ai::build_agent_system_prompt();
        let prompt = crate::ai::agent_user_prompt(
            &runtime.session.borrow().build_user_prompt(),
            if cwd.is_empty() { "." } else { &cwd },
            &runtime.shell,
            std::env::consts::OS,
            git.as_ref(),
            runtime.block_context.borrow().as_ref(),
        );
        let session_cancellation = runtime.session.borrow().cancellation_token();
        let request_cancellation = crate::ai::AiCancellationToken::new();
        *runtime.request_cancellation.borrow_mut() = Some(request_cancellation.clone());

        runtime.busy.set(true);
        runtime.sync_controls();
        let (next_turn, max_turns) = {
            let session = runtime.session.borrow();
            (session.turns_used() + 1, session.max_turns())
        };
        runtime.set_status(
            &format!(
                "Thinking with {} · turn {}/{}",
                client.display_name(),
                next_turn,
                max_turns
            ),
            true,
        );

        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        std::thread::spawn(move || {
            if session_cancellation.is_cancelled() {
                return;
            }
            let result = client.send_turns_blocking_cancellable(
                Some(&system),
                &[crate::ai::Turn {
                    role: crate::ai::Role::User,
                    text: prompt,
                }],
                &request_cancellation,
            );
            if !session_cancellation.is_cancelled() {
                let _ = tx.send(result);
            }
        });

        let rx = RefCell::new(rx);
        gtk4::glib::timeout_add_local(std::time::Duration::from_millis(50), move || {
            if !runtime.alive.get() {
                return gtk4::glib::ControlFlow::Break;
            }
            match rx.borrow().try_recv() {
                Ok(Ok(reply)) => {
                    runtime.request_cancellation.borrow_mut().take();
                    runtime.busy.set(false);
                    let outcome = runtime.session.borrow_mut().accept_model_reply(&reply);
                    match outcome {
                        Ok(ModelOutcome::Proposal {
                            id,
                            command,
                            danger,
                        }) => match crate::review_input::validate(&command) {
                            Ok(_) => Self::render_proposal(&runtime, id, command, danger),
                            Err(error) => {
                                // Keep the app boundary fail-closed even though
                                // jagent also rejects visual spoofing. Consume
                                // any unsafe pending proposal without rendering
                                // or executing its deceptive command text.
                                let _ = runtime.session.borrow_mut().reject(id);
                                let message = format!("Agent proposed an unsafe command: {error}");
                                runtime.append("Protocol error", &message);
                                runtime.render_session_state(Some(&message));
                            }
                        },
                        Ok(ModelOutcome::Said(message)) => {
                            runtime.append("Agent", &message);
                            runtime.render_session_state(None);
                        }
                        Ok(ModelOutcome::Completed(message)) => {
                            runtime.append("Agent", &message);
                            runtime.render_session_state(None);
                        }
                        Err(error) => {
                            let message = error.to_string();
                            runtime.append("Protocol error", &message);
                            runtime.render_session_state(Some(&message));
                        }
                    }
                    gtk4::glib::ControlFlow::Break
                }
                Ok(Err(error)) => {
                    runtime.request_cancellation.borrow_mut().take();
                    runtime.busy.set(false);
                    let stopped = matches!(error, crate::ai::AiError::Cancelled);
                    let message = if stopped {
                        "Model request stopped. Retry it or revise the instruction.".to_string()
                    } else {
                        error.to_string()
                    };
                    let _ = runtime.session.borrow_mut().model_failed(&message);
                    runtime.append(if stopped { "Stopped" } else { "Error" }, &message);
                    runtime.render_session_state(Some(&message));
                    gtk4::glib::ControlFlow::Break
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => gtk4::glib::ControlFlow::Continue,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    runtime.request_cancellation.borrow_mut().take();
                    runtime.busy.set(false);
                    let message = "Agent worker disconnected.";
                    let _ = runtime.session.borrow_mut().model_failed(message);
                    runtime.append("Error", message);
                    runtime.render_session_state(Some(message));
                    gtk4::glib::ControlFlow::Break
                }
            }
        });
    }

    fn render_proposal(
        runtime: &Rc<Self>,
        id: ProposalId,
        command: String,
        _danger: Option<&'static str>,
    ) {
        // Auto-execution was retired: command text cannot prove what a user's
        // shell aliases/functions or configured helpers will actually run.
        Self::render_proposal_card(runtime, id, command);
    }

    /// The manual review card, without the auto-approve fast path. Restored
    /// sessions use this directly so a proposal saved before a restart can
    /// never execute without a fresh explicit click.
    fn render_proposal_card(runtime: &Rc<Self>, id: ProposalId, command: String) {
        let epoch = runtime.task_epoch.get();
        runtime.clear_proposal();
        runtime.proposal_box.set_visible(true);
        let review = CommandReviewCard::new(CommandReviewSpec {
            presentation: ReviewPresentation::Embedded,
            compact: runtime.config.borrow().block_compact,
            icon: "\u{f544}", // nf-fa-robot
            title: "Command proposal".to_string(),
            badge: format!("Shell Agent · #{}", id.get()),
            description: "Edit or copy the proposal, insert it for manual review without running, reject it, or explicitly approve execution.".to_string(),
            command,
            primary_label: "Approve & Run".to_string(),
            primary_executes: true,
            auxiliary_label: Some("Insert only".to_string()),
            secondary_label: Some("Reject".to_string()),
            close_button: false,
        });
        runtime.proposal_box.append(&review.root);
        runtime.render_session_state(None);
        review.focus();

        let weak = Rc::downgrade(runtime);
        let entry_for_approve = review.entry.clone();
        review.primary.connect_clicked(move |_| {
            if let Some(runtime) = weak.upgrade() {
                Self::approve(runtime, id, entry_for_approve.text().to_string(), epoch);
            }
        });
        // Enter in the command entry approves & runs; dangerous commands
        // still go through the extra confirmation dialog in `approve`.
        let weak = Rc::downgrade(runtime);
        review.entry.connect_activate(move |entry| {
            if let Some(runtime) = weak.upgrade() {
                Self::approve(runtime, id, entry.text().to_string(), epoch);
            }
        });
        if let Some(insert) = review.auxiliary.as_ref() {
            let weak = Rc::downgrade(runtime);
            let entry = review.entry.clone();
            let feedback = review.feedback.clone();
            insert.connect_clicked(move |_| {
                if let Some(runtime) = weak.upgrade() {
                    Self::insert_for_manual_review(runtime, id, &entry, &feedback, epoch);
                }
            });
        }
        if let Some(reject) = review.secondary.as_ref() {
            let weak = Rc::downgrade(runtime);
            reject.connect_clicked(move |_| {
                if let Some(runtime) = weak.upgrade() {
                    Self::reject(runtime, id, epoch);
                }
            });
        }
    }

    fn insert_for_manual_review(
        runtime: Rc<Self>,
        id: ProposalId,
        entry: &Entry,
        feedback: &Label,
        epoch: u64,
    ) {
        if !runtime.proposal_callback_is_current(epoch, id) {
            log::debug!("ignored stale Agent proposal callback");
            return;
        }
        let command = match crate::review_input::validate(&entry.text()) {
            Ok(command) => command.to_string(),
            Err(error) => {
                set_review_feedback(feedback, &format!("Cannot insert: {error}"), true);
                return;
            }
        };
        let prompt_status = runtime.target.command_prompt_status();
        if !prompt_status.is_ready() {
            set_review_feedback(feedback, prompt_status.blocked_message(), true);
            return;
        }
        let Some(rollback_snapshot) = runtime.session.borrow().snapshot() else {
            set_review_feedback(
                feedback,
                "Cannot preserve the Agent proposal before inserting it.",
                true,
            );
            return;
        };
        let command = match runtime
            .session
            .borrow_mut()
            .edit_for_manual_review(id, command)
        {
            Ok(command) => command,
            Err(error) => {
                set_review_feedback(feedback, &error.to_string(), true);
                return;
            }
        };
        if let Err(error) = runtime.target.write_input(command.as_bytes()) {
            match restore_agent_snapshot(rollback_snapshot) {
                Ok(session) => *runtime.session.borrow_mut() = session,
                Err(restore_error) => {
                    runtime.session.borrow_mut().cancel();
                    log::error!(
                        "could not restore Agent proposal after PTY backpressure: {restore_error}"
                    );
                }
            }
            set_review_feedback(
                feedback,
                &format!("Command was not inserted: {error}"),
                true,
            );
            runtime.render_session_state(None);
            return;
        }
        runtime.clear_proposal();
        runtime.append(
            "You",
            "Moved the proposal to the shell prompt for manual review. The Agent did not run it and will not assume a result.",
        );
        runtime.render_session_state(Some(
            "Command inserted for manual review; edit or run it in the normal prompt.",
        ));
        runtime.target.grab_focus();
    }

    fn approve(runtime: Rc<Self>, id: ProposalId, command: String, epoch: u64) {
        if !runtime.proposal_callback_is_current(epoch, id) {
            log::debug!("ignored stale Agent proposal callback");
            return;
        }
        let command = match crate::review_input::validate(&command) {
            Ok(command) => command.to_string(),
            Err(error) => {
                runtime.set_status(&format!("Cannot approve: {error}"), false);
                return;
            }
        };
        if let Some(reason) = crate::agent::is_dangerous(&command) {
            Self::confirm_dangerous_approval(runtime, id, command, reason, epoch);
            return;
        }
        Self::approve_validated(runtime, id, command, epoch);
    }

    fn confirm_dangerous_approval(
        runtime: Rc<Self>,
        id: ProposalId,
        command: String,
        reason: &'static str,
        epoch: u64,
    ) {
        if !runtime.proposal_callback_is_current(epoch, id) {
            log::debug!("ignored stale Agent proposal callback");
            return;
        }
        let dialog = adw::AlertDialog::new(
            Some("Run a potentially destructive command?"),
            Some(&format!(
                "{reason}. Verify the exact command below before continuing."
            )),
        );
        dialog.add_responses(&[("cancel", "Cancel"), ("run", "Run Command")]);
        dialog.set_default_response(Some("cancel"));
        dialog.set_close_response("cancel");
        dialog.set_response_appearance("run", adw::ResponseAppearance::Destructive);
        let preview = Label::new(Some(&command));
        preview.set_selectable(true);
        preview.set_wrap(true);
        preview.set_xalign(0.0);
        preview.add_css_class("agent-danger-command");
        dialog.set_extra_child(Some(&preview));
        let weak = Rc::downgrade(&runtime);
        dialog.connect_response(None, move |_, response| {
            if response == "run" {
                if let Some(runtime) = weak.upgrade() {
                    Self::approve_validated(runtime, id, command.clone(), epoch);
                }
            }
        });
        dialog.present(Some(&runtime.proposal_box));
    }

    fn approve_validated(runtime: Rc<Self>, id: ProposalId, command: String, epoch: u64) {
        if !runtime.proposal_callback_is_current(epoch, id) {
            log::debug!("ignored stale Agent proposal callback");
            return;
        }
        let prompt_status = runtime.target.agent_command_prompt_status();
        if !prompt_status.is_ready() {
            let message = prompt_status.blocked_message();
            runtime.set_status(message, false);
            runtime.append("Safety check", message);
            return;
        }
        let Some(rollback_snapshot) = runtime.session.borrow().snapshot() else {
            runtime.set_status("Cannot preserve the Agent proposal before approval.", false);
            return;
        };
        let approval_result = runtime.session.borrow_mut().edit_and_approve(id, command);
        let approved = match approval_result {
            Ok(approved) => approved,
            Err(error) => {
                runtime.set_status(&error.to_string(), false);
                return;
            }
        };
        // No visible "approved" message: the approved command runs immediately
        // and its real finished block lands in the conversation right here.
        let Some(generation) = runtime.next_execution_generation.get().checked_add(1) else {
            runtime.session.borrow_mut().cancel();
            runtime.render_session_state(Some("Agent execution identities are exhausted."));
            return;
        };
        runtime.next_execution_generation.set(generation);
        *runtime.pending_command.borrow_mut() = Some(PendingExecution {
            value: approved.proposal_id,
            command: approved.command.clone(),
            generation,
        });
        runtime.render_session_state(None);
        runtime.target.grab_focus();
        if let Err(error) = runtime
            .target
            .submit_agent_command(generation, &approved.command)
        {
            runtime.pending_command.borrow_mut().take();
            match restore_agent_snapshot(rollback_snapshot) {
                Ok(session) => *runtime.session.borrow_mut() = session,
                Err(restore_error) => {
                    runtime.session.borrow_mut().cancel();
                    log::error!(
                        "could not restore Agent approval after PTY backpressure: {restore_error}"
                    );
                }
            }
            runtime.render_session_state(Some(&format!("Command was not sent: {error}")));
            return;
        }
        runtime.clear_proposal();
    }

    fn reject(runtime: Rc<Self>, id: ProposalId, epoch: u64) {
        if !runtime.proposal_callback_is_current(epoch, id) {
            log::debug!("ignored stale Agent proposal callback");
            return;
        }
        let result = runtime.session.borrow_mut().reject(id);
        match result {
            Ok(()) => {
                runtime.clear_proposal();
                runtime.append("You", "Rejected proposal; ask for another approach.");
                Self::request_model(runtime);
            }
            Err(error) => runtime.render_session_state(Some(&error.to_string())),
        }
    }

    fn observe(
        runtime: Rc<Self>,
        command: String,
        exit_code: Option<i32>,
        output: String,
        agent_generation: Option<u64>,
    ) {
        let completion = {
            let mut session = runtime.session.borrow_mut();
            let mut pending = runtime.pending_command.borrow_mut();
            resolve_pending_for_finished_block(
                &mut session,
                &mut pending,
                &command,
                agent_generation,
            )
        };
        let id = match completion {
            PendingCompletion::Matched(id) => id,
            PendingCompletion::Unrelated => return,
            PendingCompletion::CommandMismatch => {
                const MESSAGE: &str =
                    "Agent stopped because command completion correlation failed.";
                runtime.clear_proposal();
                runtime.append("Safety check", MESSAGE);
                runtime.render_session_state(None);
                runtime.set_status(MESSAGE, false);
                return;
            }
        };
        // The jagent observation turn carries a concrete exit code (frozen
        // API). An unreported status becomes the sentinel plus a note the
        // model can actually read, never the successful 0 it used to be.
        let (exit_code, unknown_note) = crate::block_view::exit_code_for_shared_surface(exit_code);
        let output = match unknown_note {
            Some(note) => format!("{note}\n{output}"),
            None => output,
        };
        let observation_result = runtime.session.borrow_mut().observe(id, exit_code, &output);
        if let Err(error) = observation_result {
            runtime.render_session_state(Some(&error.to_string()));
            return;
        }
        // The command's own finished block already shows the result in the
        // conversation; only the session (model context) records it here.
        Self::request_model(runtime);
    }

    fn execution_lost(runtime: Rc<Self>, generation: u64, reason: &'static str) {
        if take_pending_for_lost_execution(&mut runtime.pending_command.borrow_mut(), generation)
            .is_none()
        {
            return;
        }
        runtime.session.borrow_mut().cancel();
        runtime.clear_proposal();
        let message = format!("Agent stopped safely because {reason}.");
        runtime.append("Safety check", &message);
        runtime.render_session_state(None);
        runtime.set_status(&message, false);
    }

    fn cancel(&self) {
        if !self.alive.replace(false) {
            return;
        }
        if let Some(cancellation) = self.request_cancellation.borrow_mut().take() {
            cancellation.cancel();
            if !cancellation.wait_for_inactive(std::time::Duration::from_millis(500)) {
                log::warn!("Timed out waiting for the Agent request to shut down");
            }
        }
        self.session.borrow_mut().cancel();
        if let Some(generation) = self
            .pending_command
            .borrow()
            .as_ref()
            .map(|pending| pending.generation)
        {
            self.target.cancel_pending_agent_submission(generation);
        }
        self.pending_command.borrow_mut().take();
        self.busy.set(false);
        self.clear_proposal();
        self.render_session_state(None);
    }

    /// Cancel the session and remove its inline card. Idempotent.
    fn shutdown(&self) {
        self.cancel();
        self.target.remove_inline_notice(&self.card);
    }
}

/// Build one Shell Agent conversation message styled like a finished block:
/// a header row identifying the Shell Agent dialogue and the speaker, then the
/// message body. It is inserted as an inline notice, so it never joins block
/// history or virtualization.
fn build_agent_message_block(speaker: &str, body: &str, compact: bool) -> gtk4::Widget {
    let error_speaker = matches!(
        speaker,
        "Error" | "Protocol error" | "Stopped" | "Safety check"
    );

    let outer = GBox::new(Orientation::Vertical, 0);
    outer.add_css_class("block-finished");
    outer.add_css_class("block-assistant");
    outer.add_css_class("block-agent");
    let accessible_name = format!(
        "Shell Agent activity: {}",
        crate::review_input::safe_inline_display(speaker, 256)
    );
    outer.update_property(&[gtk4::accessible::Property::Label(&accessible_name)]);
    outer.set_hexpand(true);
    outer.set_vexpand(false);
    if compact {
        outer.add_css_class("block-compact");
        outer.set_margin_top(1);
        outer.set_margin_bottom(1);
        outer.set_margin_start(4);
        outer.set_margin_end(4);
    } else {
        outer.set_margin_top(4);
        outer.set_margin_bottom(4);
        outer.set_margin_start(8);
        outer.set_margin_end(8);
    }

    let header = GBox::new(Orientation::Horizontal, 8);
    header.add_css_class("block-header");
    if compact {
        header.set_margin_start(8);
        header.set_margin_end(6);
        header.set_margin_top(3);
        header.set_margin_bottom(1);
    } else {
        header.set_margin_start(12);
        header.set_margin_end(8);
        header.set_margin_top(6);
        header.set_margin_bottom(2);
    }
    let icon = Label::new(Some(if speaker == "You" {
        "\u{f007}" // nf-fa-user
    } else {
        "\u{f544}" // nf-fa-robot
    }));
    icon.add_css_class("agent-card-icon");
    icon.add_css_class("assistant-card-icon");
    header.append(&icon);
    let title = Label::new(Some("Shell Agent"));
    title.add_css_class("agent-card-title");
    title.add_css_class("assistant-card-title");
    title.set_xalign(0.0);
    header.append(&title);
    let speaker = crate::review_input::safe_inline_display(speaker, 256);
    let speaker_chip = Label::new(Some(&speaker));
    speaker_chip.add_css_class("agent-chip");
    if error_speaker {
        speaker_chip.add_css_class("agent-msg-error");
    }
    speaker_chip.set_halign(gtk4::Align::Start);
    speaker_chip.set_hexpand(true);
    header.append(&speaker_chip);
    outer.append(&header);

    let body = agent_message_display_text(body);
    let body_label = Label::new(Some(&body));
    body_label.add_css_class("agent-msg-body");
    body_label.set_xalign(0.0);
    body_label.set_wrap(true);
    body_label.set_wrap_mode(gtk4::pango::WrapMode::WordChar);
    body_label.set_selectable(true);
    body_label.set_margin_start(if compact { 8 } else { 12 });
    body_label.set_margin_end(if compact { 8 } else { 12 });
    body_label.set_margin_top(2);
    body_label.set_margin_bottom(if compact { 6 } else { 10 });
    outer.append(&body_label);

    outer.upcast()
}

fn agent_message_display_text(body: &str) -> String {
    crate::review_input::safe_multiline_display(body, MAX_AGENT_MESSAGE_DISPLAY_BYTES)
}

fn agent_message_display_bytes(speaker: &str, body: &str) -> usize {
    let speaker = crate::review_input::safe_inline_display(speaker, 256);
    let body = agent_message_display_text(body);
    "Shell Agent"
        .len()
        .saturating_add('\u{f007}'.len_utf8())
        .saturating_add(speaker.len())
        .saturating_add(body.len())
        .saturating_add("Shell Agent activity: ".len())
        .saturating_add(speaker.len())
}

fn bounded_agent_input(mut text: String) -> String {
    if text.len() <= MAX_AGENT_INPUT_BYTES {
        return text;
    }
    let mut end = MAX_AGENT_INPUT_BYTES;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text.truncate(end);
    text
}

fn agent_block_context_label(context: &crate::ai::BlockContext) -> String {
    let truncation = if context.truncated {
        " · output truncated"
    } else {
        ""
    };
    format!(
        "Selected Block · exit {}{truncation} · {}",
        context.exit_code,
        compact_one_line(&context.cmd, 56)
    )
}

fn agent_block_context_tooltip(context: &crate::ai::BlockContext) -> String {
    let cwd = crate::review_input::safe_inline_display(
        context.cwd.as_deref().unwrap_or("unknown cwd"),
        4 * 1024,
    );
    format!(
        "Attached as untrusted context\nexit: {}\noutput: {}\ncwd: {cwd}\ncommand: {}",
        context.exit_code,
        if context.truncated {
            "truncated"
        } else {
            "complete"
        },
        compact_one_line(&context.cmd, 160)
    )
}

fn compact_one_line(text: &str, max_chars: usize) -> String {
    let text = crate::review_input::safe_inline_display(text, 16 * 1024);
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut chars = collapsed.chars();
    let preview: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{preview}…")
    } else if preview.is_empty() {
        "(empty command)".to_string()
    } else {
        preview
    }
}

/// Configuration-type Shell Agent content lives in this small dialog, opened
/// from the inline card's header: identity, provider/shell chips, and the AI
/// command-correction toggle. Session activity never renders here.
fn show_agent_settings_dialog(ui: &UiState, cwd: &str, shell: &str) {
    let (provider, model, correction_enabled) = {
        let config = ui.config.borrow();
        (
            config.ai_provider.clone(),
            config.ai_model.clone(),
            config.command_correction_enabled,
        )
    };

    let dialog = adw::Dialog::builder()
        .title("Shell Agent settings")
        .content_width(620)
        .build();
    let header = adw::HeaderBar::new();

    let overview = GBox::new(Orientation::Vertical, 8);
    overview.add_css_class("agent-overview");
    let identity_row = GBox::new(Orientation::Horizontal, 10);
    let agent_icon = Image::from_icon_name("system-run-symbolic");
    agent_icon.set_pixel_size(32);
    agent_icon.add_css_class("agent-icon");
    let identity_copy = GBox::new(Orientation::Vertical, 2);
    identity_copy.set_hexpand(true);
    let title = Label::new(Some("Approval-gated shell assistant"));
    title.set_xalign(0.0);
    title.add_css_class("title-3");
    let cwd = crate::review_input::safe_inline_display(cwd, 4 * 1024);
    let shell = crate::review_input::safe_inline_display(shell, 4 * 1024);
    let provider = crate::review_input::safe_inline_display(&provider, 256);
    let model = crate::review_input::safe_inline_display(&model, 512);
    let target_label = Label::new(Some(&format!("Bound to Block pane · {cwd}")));
    target_label.set_xalign(0.0);
    target_label.set_ellipsize(gtk4::pango::EllipsizeMode::Middle);
    target_label.set_tooltip_text(Some(&cwd));
    target_label.add_css_class("dim-label");
    identity_copy.append(&title);
    identity_copy.append(&target_label);
    identity_row.append(&agent_icon);
    identity_row.append(&identity_copy);
    overview.append(&identity_row);

    let chips = GBox::new(Orientation::Horizontal, 6);
    let provider_chip = Label::new(Some(&format!("{provider} · {model}")));
    provider_chip.set_hexpand(true);
    // Keep the pill hugging its text; hexpand alone stretches the
    // background into a long empty capsule.
    provider_chip.set_halign(gtk4::Align::Start);
    provider_chip.set_max_width_chars(44);
    provider_chip.set_ellipsize(gtk4::pango::EllipsizeMode::End);
    provider_chip.set_tooltip_text(Some(&format!("{provider} · {model}")));
    provider_chip.add_css_class("agent-chip");
    let shell_chip = Label::new(Some(&format!("shell: {shell}")));
    shell_chip.set_max_width_chars(26);
    shell_chip.set_ellipsize(gtk4::pango::EllipsizeMode::Middle);
    shell_chip.set_tooltip_text(Some(&shell));
    shell_chip.add_css_class("agent-chip");
    let safety_chip = Label::new(Some("Review required"));
    safety_chip.add_css_class("agent-chip");
    safety_chip.add_css_class("agent-safety-chip");
    chips.append(&provider_chip);
    chips.append(&shell_chip);
    chips.append(&safety_chip);
    overview.append(&chips);

    let correction_row = GBox::new(Orientation::Horizontal, 12);
    correction_row.add_css_class("agent-setting-card");
    let correction_copy = GBox::new(Orientation::Vertical, 2);
    correction_copy.set_hexpand(true);
    let correction_title = Label::new(Some("AI command correction"));
    correction_title.set_xalign(0.0);
    correction_title.add_css_class("heading");
    let correction_hint = Label::new(Some(
        "After typo-like Block failures, offer an editable correction; never run it automatically.",
    ));
    correction_hint.set_xalign(0.0);
    correction_hint.set_wrap(true);
    correction_hint.add_css_class("dim-label");
    correction_copy.append(&correction_title);
    correction_copy.append(&correction_hint);
    let correction_switch = Switch::builder()
        .active(correction_enabled)
        .valign(gtk4::Align::Center)
        .build();
    correction_switch.set_tooltip_text(Some("Enable review-first command correction"));
    correction_row.append(&correction_copy);
    correction_row.append(&correction_switch);

    let ui_for_correction = ui.clone();
    correction_switch.connect_active_notify(move |toggle| {
        ui_for_correction
            .config
            .borrow_mut()
            .command_correction_enabled = toggle.is_active();
        ui_for_correction.persist_config();
    });

    let auto_row = GBox::new(Orientation::Horizontal, 12);
    auto_row.add_css_class("agent-setting-card");
    let auto_copy = GBox::new(Orientation::Vertical, 2);
    auto_copy.set_hexpand(true);
    let auto_title = Label::new(Some("Automatic command execution retired"));
    auto_title.set_xalign(0.0);
    auto_title.add_css_class("heading");
    let auto_hint = Label::new(Some(
        "Every Agent proposal requires explicit approval. Command text cannot prove what \
         aliases, functions, configured helpers, or tool flags will actually execute.",
    ));
    auto_hint.set_xalign(0.0);
    auto_hint.set_wrap(true);
    auto_hint.add_css_class("dim-label");
    auto_copy.append(&auto_title);
    auto_copy.append(&auto_hint);
    let auto_switch = Switch::builder()
        .active(false)
        .valign(gtk4::Align::Center)
        .build();
    auto_switch.set_sensitive(false);
    auto_switch.set_tooltip_text(Some("Automatic execution is disabled for safety"));
    auto_row.append(&auto_copy);
    auto_row.append(&auto_switch);

    let body = GBox::new(Orientation::Vertical, 10);
    body.add_css_class("agent-dashboard");
    body.set_margin_start(12);
    body.set_margin_end(12);
    body.set_margin_top(10);
    body.set_margin_bottom(12);
    body.append(&overview);
    body.append(&auto_row);
    body.append(&correction_row);
    let toolbar = adw::ToolbarView::new();
    toolbar.add_css_class("agent-surface");
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&body));
    dialog.set_child(Some(&toolbar));
    dialog.present(Some(&ui.window));
}

impl UiState {
    /// Install callbacks once per TermView and route events to whichever Agent
    /// runtime is current for this window. Session reopen therefore changes
    /// only the slot contents, never the terminal's callback vector.
    fn ensure_agent_event_bridge(&self, target: &Rc<TermView>) {
        if !self.agent_ui_lifetime.claim_event_bridge(target) {
            return;
        }

        let target_weak = Rc::downgrade(target);
        let slot = self.agent_session.clone();
        target.connect_block_finished(
            move |command, exit_code, output, agent_generation, _duration_ms| {
                let Some(target) = target_weak.upgrade() else {
                    return;
                };
                let runtime = slot.borrow().as_ref().and_then(|session| {
                    Rc::ptr_eq(&session.runtime.target, &target).then(|| session.runtime.clone())
                });
                if let Some(runtime) = runtime {
                    // The freshly finished block was inserted below this card;
                    // re-pin the current card directly above the live prompt.
                    runtime.target.insert_inline_notice(&runtime.card);
                    AgentRuntime::observe(runtime, command, exit_code, output, agent_generation);
                }
            },
        );

        let target_weak = Rc::downgrade(target);
        let slot = self.agent_session.clone();
        target.connect_agent_execution_lost(move |generation, reason| {
            let Some(target) = target_weak.upgrade() else {
                return;
            };
            let runtime = slot.borrow().as_ref().and_then(|session| {
                Rc::ptr_eq(&session.runtime.target, &target).then(|| session.runtime.clone())
            });
            if let Some(runtime) = runtime {
                AgentRuntime::execution_lost(runtime, generation, reason);
            }
        });

        let target_weak = Rc::downgrade(target);
        let slot = self.agent_session.clone();
        let toggle = self.agent_toggle.clone();
        target.connect_exited(move |_| {
            let Some(target) = target_weak.upgrade() else {
                return;
            };
            let runtime = {
                let mut slot = slot.borrow_mut();
                let is_current_target = slot
                    .as_ref()
                    .is_some_and(|session| Rc::ptr_eq(&session.runtime.target, &target));
                if is_current_target {
                    slot.take().map(|session| session.runtime)
                } else {
                    None
                }
            };
            if let Some(runtime) = runtime {
                toggle.set_active(false);
                runtime.shutdown();
            }
        });
    }

    /// Keep the visible top-bar Agent control aligned with both configuration
    /// availability and the lifetime of the active Agent session.
    pub(crate) fn sync_agent_toggle(&self) {
        let (ai_available, available) = {
            let config = self.config.borrow();
            (config.ai_enabled, config.ai_enabled && config.agent_enabled)
        };
        self.agent_toggle.set_sensitive(available);

        if !ai_available {
            let suggestion = self.command_suggestion.borrow_mut().take();
            if let Some(suggestion) = suggestion {
                suggestion.shutdown();
            }
        }
        if !available {
            // Take the handle out of the slot before shutdown so anything
            // observing the slot already sees the session as closed.
            let session = self.agent_session.borrow_mut().take();
            if let Some(session) = session {
                session.shutdown();
            }
            self.agent_toggle.set_active(false);
        } else {
            self.agent_toggle
                .set_active(self.agent_session.borrow().is_some());
        }
    }

    pub(crate) fn toggle_agent_panel(&self) {
        // Toggle off: close the active inline session.
        let existing = self.agent_session.borrow_mut().take();
        if let Some(session) = existing {
            session.shutdown();
            self.agent_toggle.set_active(false);
            return;
        }
        let config = self.config.borrow();
        if !config.ai_enabled || !config.agent_enabled {
            drop(config);
            self.agent_toggle.set_active(false);
            self.show_ai_error("Agent mode is disabled in Settings or safe mode.");
            return;
        }
        let max_turns = config.agent_max_turns;
        let compact = config.block_compact;
        drop(config);
        if crate::host::is_flatpak() {
            self.agent_toggle.set_active(false);
            self.show_ai_error(
                "Shell Agent execution is unavailable through the Flatpak host bridge because Forge cannot verify host-side foreground command ownership. AI Chat and review-only correction remain available.",
            );
            return;
        }
        let Some(current_leaf) = self.current_pane_leaf() else {
            self.agent_toggle.set_active(false);
            self.show_ai_error("Agent mode requires an active Block pane.");
            return;
        };
        if current_leaf.is_remote() {
            self.agent_toggle.set_active(false);
            self.show_ai_error(
                "Shell Agent execution is unavailable in managed remote panes because Forge cannot authenticate remote terminal lifecycle markers.",
            );
            return;
        }
        let Some(target) = current_leaf.block_view() else {
            self.agent_toggle.set_active(false);
            self.show_ai_error("Agent mode requires an active Block pane.");
            return;
        };
        self.ensure_agent_event_bridge(&target);
        let block_context = target.selected_block_context(80);
        let cwd = target.cwd();
        let cwd = if cwd.is_empty() { ".".to_string() } else { cwd };
        let shell = self
            .shell_argv
            .borrow()
            .first()
            .cloned()
            .unwrap_or_else(|| "sh".to_string());

        // ── Inline agent card, styled like a block ────────────────────────
        let outer = GBox::new(Orientation::Vertical, 0);
        outer.add_css_class("block-finished");
        outer.add_css_class("block-assistant");
        outer.add_css_class("block-agent");
        outer.update_property(&[gtk4::accessible::Property::Label("Shell Agent session")]);
        outer.set_hexpand(true);
        outer.set_vexpand(false);
        if compact {
            outer.add_css_class("block-compact");
            outer.set_margin_top(1);
            outer.set_margin_bottom(1);
            outer.set_margin_start(4);
            outer.set_margin_end(4);
        } else {
            outer.set_margin_top(4);
            outer.set_margin_bottom(4);
            outer.set_margin_start(8);
            outer.set_margin_end(8);
        }

        let header = GBox::new(Orientation::Horizontal, 8);
        header.add_css_class("block-header");
        if compact {
            header.set_margin_start(8);
            header.set_margin_end(6);
            header.set_margin_top(3);
            header.set_margin_bottom(1);
        } else {
            header.set_margin_start(12);
            header.set_margin_end(8);
            header.set_margin_top(6);
            header.set_margin_bottom(2);
        }
        let icon = Label::new(Some("\u{f544}")); // nf-fa-robot
        icon.add_css_class("agent-card-icon");
        icon.add_css_class("assistant-card-icon");
        header.append(&icon);
        let title = Label::new(Some("Shell Agent"));
        title.add_css_class("agent-card-title");
        title.add_css_class("assistant-card-title");
        title.set_xalign(0.0);
        header.append(&title);
        let cwd = crate::review_input::safe_inline_display(&cwd, 4 * 1024);
        let binding_label = Label::new(Some(&format!(
            "{cwd} · review required · every command needs approval"
        )));
        binding_label.add_css_class("agent-card-binding");
        binding_label.add_css_class("assistant-card-badge");
        binding_label.set_hexpand(true);
        binding_label.set_halign(gtk4::Align::End);
        binding_label.set_ellipsize(gtk4::pango::EllipsizeMode::Middle);
        binding_label.set_tooltip_text(Some(&cwd));
        header.append(&binding_label);
        let settings_btn = Button::with_label("\u{f013}"); // nf-fa-cog
        settings_btn.add_css_class("flat");
        settings_btn.set_focusable(false);
        settings_btn.set_tooltip_text(Some("Shell Agent settings"));
        settings_btn.update_property(&[gtk4::accessible::Property::Label("Shell Agent settings")]);
        header.append(&settings_btn);
        let close_btn = Button::with_label("\u{2715}");
        close_btn.add_css_class("flat");
        close_btn.set_focusable(false);
        close_btn.set_tooltip_text(Some("Cancel Agent and close this card"));
        close_btn.update_property(&[gtk4::accessible::Property::Label(
            "Cancel Shell Agent session",
        )]);
        header.append(&close_btn);
        outer.append(&header);

        let context_card = GBox::new(Orientation::Horizontal, 8);
        context_card.add_css_class("agent-context-card");
        let context_label = Label::new(
            block_context
                .as_ref()
                .map(agent_block_context_label)
                .as_deref(),
        );
        context_label.set_xalign(0.0);
        context_label.set_hexpand(true);
        context_label.set_ellipsize(gtk4::pango::EllipsizeMode::Middle);
        context_label.set_tooltip_text(
            block_context
                .as_ref()
                .map(agent_block_context_tooltip)
                .as_deref(),
        );
        let context_clear = Button::from_icon_name("window-close-symbolic");
        context_clear.add_css_class("flat");
        context_clear.set_tooltip_text(Some("Detach selected Block context"));
        context_clear.update_property(&[gtk4::accessible::Property::Label(
            "Detach selected Block context",
        )]);
        context_card.append(&context_label);
        context_card.append(&context_clear);
        context_card.set_visible(block_context.is_some());

        let status = Label::new(Some("Ready for the next instruction"));
        status.set_xalign(0.0);
        status.set_wrap(true);
        status.set_hexpand(true);
        status.add_css_class("agent-status");
        status.set_accessible_role(gtk4::AccessibleRole::Status);
        let status_spinner = Spinner::new();
        status_spinner.set_spinning(false);
        let turn_label = Label::new(Some(&format!("0 / {max_turns} turns")));
        turn_label.add_css_class("dim-label");
        turn_label.add_css_class("agent-turn-label");
        let retry_request = Button::with_label("Retry");
        retry_request.set_visible(false);
        retry_request.set_tooltip_text(Some(
            "Retry the failed model turn without duplicating input",
        ));
        let stop_request = Button::with_label("Stop");
        stop_request.set_visible(false);
        stop_request.add_css_class("destructive-action");
        stop_request.set_tooltip_text(Some("Stop this model request and keep the Agent session"));
        let session_action = Button::with_label("Follow up");
        session_action.set_visible(false);
        session_action.set_tooltip_text(Some(
            "Continue the completed task or start a fresh task when the turn limit is reached",
        ));
        let prompt_status = Label::new(Some("Prompt initializing"));
        prompt_status.add_css_class("agent-prompt-status");
        prompt_status.add_css_class("agent-prompt-blocked");
        prompt_status.set_accessible_role(gtk4::AccessibleRole::Status);
        let turn_progress = ProgressBar::new();
        turn_progress.set_hexpand(true);
        turn_progress.set_fraction(0.0);
        let status_top = GBox::new(Orientation::Horizontal, 8);
        status_top.append(&status_spinner);
        status_top.append(&status);
        status_top.append(&retry_request);
        status_top.append(&stop_request);
        status_top.append(&session_action);
        status_top.append(&prompt_status);
        status_top.append(&turn_label);
        let status_card = GBox::new(Orientation::Vertical, 6);
        status_card.add_css_class("agent-status-card");
        status_card.append(&status_top);
        status_card.append(&turn_progress);

        let proposal_box = GBox::new(Orientation::Vertical, 8);
        proposal_box.add_css_class("agent-proposal-card");
        proposal_box.set_visible(false);

        let input = Entry::new();
        input.set_hexpand(true);
        input.set_placeholder_text(Some(if block_context.is_some() {
            "Ask about the selected Block or describe a task…"
        } else {
            "Describe a task for this pane…"
        }));
        input.add_css_class("agent-input");
        input.update_property(&[gtk4::accessible::Property::Label("Shell Agent instruction")]);
        let send = Button::with_label("Send");
        send.set_sensitive(false);
        send.add_css_class("suggested-action");
        send.add_css_class("agent-send");
        let input_row = GBox::new(Orientation::Horizontal, 6);
        input_row.append(&input);
        input_row.append(&send);
        let input_hint = Label::new(Some(
            "Enter sends · every proposed command stays editable and requires approval",
        ));
        input_hint.set_xalign(0.0);
        input_hint.set_hexpand(true);
        input_hint.add_css_class("dim-label");
        input_hint.add_css_class("agent-input-hint");
        let context_attach = Button::with_label("Attach selected Block");
        context_attach.add_css_class("flat");
        context_attach.set_tooltip_text(Some(
            "Attach the selected finished Block in this pane as untrusted context \
             for upcoming instructions (replaces a previously attached Block)",
        ));
        let hint_row = GBox::new(Orientation::Horizontal, 6);
        hint_row.append(&input_hint);
        hint_row.append(&context_attach);
        let composer = GBox::new(Orientation::Vertical, 6);
        composer.add_css_class("agent-composer");
        composer.append(&input_row);
        composer.append(&hint_row);

        let body = GBox::new(Orientation::Vertical, 8);
        body.set_margin_start(if compact { 8 } else { 12 });
        body.set_margin_end(if compact { 8 } else { 12 });
        body.set_margin_top(2);
        body.set_margin_bottom(if compact { 6 } else { 10 });
        body.append(&context_card);
        body.append(&proposal_box);
        body.append(&status_card);
        body.append(&composer);
        outer.append(&body);

        let card: gtk4::Widget = outer.clone().upcast();
        // A snapshot persisted by the previous run is atomically claimed,
        // consumed once, and rebound to the pane the user opened the Agent on.
        // Pending approval state must never be restorable by two processes.
        let restored_session = load_agent_snapshot(&agent_snapshot_path());
        let was_restored = restored_session.is_some();
        let runtime = Rc::new(AgentRuntime {
            session: RefCell::new(restored_session.unwrap_or_else(|| AgentSession::new(max_turns))),
            target: target.clone(),
            ui_lifetime: self.agent_ui_lifetime.clone(),
            config: self.config.clone(),
            shell: shell.clone(),
            block_context: RefCell::new(block_context.clone()),
            card: card.clone(),
            input: input.clone(),
            send: send.clone(),
            stop_request: stop_request.clone(),
            retry_request: retry_request.clone(),
            context_clear: context_clear.clone(),
            context_attach: context_attach.clone(),
            context_card: context_card.clone(),
            context_label: context_label.clone(),
            status,
            status_spinner,
            prompt_status,
            turn_progress,
            turn_label,
            session_action: session_action.clone(),
            proposal_box,
            task_epoch: Cell::new(0),
            pending_command: RefCell::new(None),
            next_execution_generation: Cell::new(0),
            request_cancellation: RefCell::new(None),
            busy: Cell::new(false),
            alive: Cell::new(true),
            organism_agent: self.organism_agent.clone(),
        });
        let intro = if block_context.is_some() {
            "Bound to this Block pane with the selected finished Block attached as untrusted context. I can propose commands, but cannot run one without your explicit approval."
        } else {
            "Bound to this Block pane. I can propose commands, but cannot run one without your explicit approval."
        };
        runtime.append("Agent", intro);
        if was_restored {
            runtime.append(
                "Agent",
                "Restored the previous agent session from your last run.",
            );
            let transcript: Vec<Turn> = runtime.session.borrow().transcript().to_vec();
            for turn in &transcript {
                match turn {
                    Turn::User(message) => runtime.append("You", message),
                    Turn::AssistantThought(thought) => runtime.append("Agent (thought)", thought),
                    Turn::AssistantSay(message) => runtime.append("Agent", message),
                    Turn::AssistantProposed {
                        id,
                        command,
                        status,
                    } => {
                        let verdict = match status {
                            ProposalStatus::Pending => "awaiting approval",
                            ProposalStatus::Approved => "approved and ran",
                            ProposalStatus::Rejected => "rejected",
                            ProposalStatus::ManualReview => "moved to manual review",
                        };
                        runtime.append(
                            "Agent",
                            &format!("Proposed command #{} ({verdict}): {command}", id.get()),
                        );
                    }
                    Turn::Observation {
                        exit_code,
                        output_sample,
                        ..
                    } => runtime.append("Output", &format!("exit {exit_code}\n{output_sample}")),
                    Turn::ProtocolError(message) => runtime.append("Error", message),
                }
            }
            // Resume what the snapshot left in flight: a pending proposal is
            // re-rendered as a manual review card (never auto-approved), and
            // a lost model turn is simply re-requested.
            let state = runtime.session.borrow().state();
            match state {
                AgentState::AwaitingApproval { proposal_id } => {
                    let pending = transcript.iter().rev().find_map(|turn| match turn {
                        Turn::AssistantProposed {
                            id,
                            command,
                            status: ProposalStatus::Pending,
                        } if *id == proposal_id => Some(command.clone()),
                        _ => None,
                    });
                    if let Some(command) = pending {
                        AgentRuntime::render_proposal_card(&runtime, proposal_id, command);
                    }
                }
                AgentState::AwaitingModel => AgentRuntime::request_model(runtime.clone()),
                _ => {}
            }
        }
        runtime.render_session_state(None);

        // Close this specific session: clear the UiState slot only when it
        // still holds this runtime, so delayed widget callbacks can never tear
        // down a newer session.
        let close_session = {
            let slot = self.agent_session.clone();
            let toggle = self.agent_toggle.clone();
            let weak = Rc::downgrade(&runtime);
            Rc::new(move || {
                let Some(runtime) = weak.upgrade() else {
                    return;
                };
                let is_current = slot
                    .borrow()
                    .as_ref()
                    .is_some_and(|session| Rc::ptr_eq(&session.runtime, &runtime));
                if is_current {
                    slot.borrow_mut().take();
                    toggle.set_active(false);
                }
                runtime.shutdown();
            })
        };

        let ui_for_settings = self.clone();
        let cwd_for_settings = cwd.clone();
        let shell_for_settings = shell.clone();
        settings_btn.connect_clicked(move |_| {
            show_agent_settings_dialog(&ui_for_settings, &cwd_for_settings, &shell_for_settings);
        });
        {
            let close_session = close_session.clone();
            close_btn.connect_clicked(move |_| close_session());
        }
        let weak = Rc::downgrade(&runtime);
        send.connect_clicked(move |_| {
            if let Some(runtime) = weak.upgrade() {
                AgentRuntime::submit(runtime);
            }
        });
        let weak = Rc::downgrade(&runtime);
        input.connect_activate(move |_| {
            if let Some(runtime) = weak.upgrade() {
                AgentRuntime::submit(runtime);
            }
        });
        let weak = Rc::downgrade(&runtime);
        input.connect_changed(move |_| {
            if let Some(runtime) = weak.upgrade() {
                let text = runtime.input.text().to_string();
                let truncated = text.len() > MAX_AGENT_INPUT_BYTES;
                let bounded = bounded_agent_input(text);
                if truncated {
                    runtime.input.set_text(&bounded);
                    runtime.input.set_position(-1);
                    runtime.set_status("Instruction was limited to 16 KiB.", false);
                }
                runtime.sync_controls();
            }
        });
        let weak = Rc::downgrade(&runtime);
        stop_request.connect_clicked(move |_| {
            if let Some(runtime) = weak.upgrade() {
                runtime.stop_current_request();
            }
        });
        let weak = Rc::downgrade(&runtime);
        retry_request.connect_clicked(move |_| {
            if let Some(runtime) = weak.upgrade() {
                AgentRuntime::retry_model(runtime);
            }
        });
        let weak = Rc::downgrade(&runtime);
        session_action.connect_clicked(move |_| {
            if let Some(runtime) = weak.upgrade() {
                AgentRuntime::resume_or_start_new(runtime);
            }
        });
        let weak = Rc::downgrade(&runtime);
        context_clear.connect_clicked(move |_| {
            if let Some(runtime) = weak.upgrade() {
                runtime.detach_block_context();
            }
        });
        let weak = Rc::downgrade(&runtime);
        context_attach.connect_clicked(move |_| {
            if let Some(runtime) = weak.upgrade() {
                runtime.attach_block_context();
            }
        });
        let weak = Rc::downgrade(&runtime);
        gtk4::glib::timeout_add_local(std::time::Duration::from_millis(250), move || {
            let Some(runtime) = weak.upgrade() else {
                return gtk4::glib::ControlFlow::Break;
            };
            if !runtime.alive.get() {
                return gtk4::glib::ControlFlow::Break;
            }
            runtime.sync_prompt_status();
            gtk4::glib::ControlFlow::Continue
        });

        *self.agent_session.borrow_mut() = Some(AgentHandle { runtime });
        self.agent_toggle.set_active(true);
        target.insert_inline_notice(&card);
        input.grab_focus();
    }
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use super::claim_agent_snapshot_file;
    use super::{
        agent_message_display_bytes, agent_message_display_text, bounded_agent_input,
        claim_weak_target, load_agent_snapshot, proposal_callback_is_current,
        read_agent_snapshot_file, resolve_pending_for_finished_block, restore_agent_snapshot,
        take_pending_for_finished_block, take_pending_for_lost_execution,
        write_agent_snapshot_file, AgentActivityBudget, PendingCompletion, PendingExecution,
        MAX_AGENT_ACTIVITY_CARDS, MAX_AGENT_ACTIVITY_DISPLAY_BYTES, MAX_AGENT_INPUT_BYTES,
        MAX_AGENT_MESSAGE_DISPLAY_BYTES,
    };

    fn pending(value: u64, command: &str) -> Option<PendingExecution<u64>> {
        Some(PendingExecution {
            value,
            command: command.to_string(),
            generation: 1,
        })
    }

    #[test]
    fn agent_activity_budget_evicts_oldest_at_the_card_limit() {
        let mut budget = AgentActivityBudget::default();
        for bytes in 0..MAX_AGENT_ACTIVITY_CARDS {
            assert_eq!(budget.push(bytes), 0);
        }

        assert_eq!(budget.push(7), 1);
        assert_eq!(budget.item_bytes.len(), MAX_AGENT_ACTIVITY_CARDS);
        assert_eq!(budget.item_bytes.front(), Some(&1));
    }

    #[test]
    fn agent_activity_budget_evicts_fifo_at_one_mib() {
        let mut budget = AgentActivityBudget::default();
        let quarter = MAX_AGENT_ACTIVITY_DISPLAY_BYTES / 4;
        for _ in 0..4 {
            assert_eq!(budget.push(quarter), 0);
        }
        assert_eq!(budget.total_display_bytes, MAX_AGENT_ACTIVITY_DISPLAY_BYTES);

        assert_eq!(budget.push(quarter), 1);
        assert_eq!(budget.item_bytes.len(), 4);
        assert_eq!(budget.total_display_bytes, MAX_AGENT_ACTIVITY_DISPLAY_BYTES);

        // A single impossible-to-display entry cannot punch through the cap.
        assert_eq!(budget.push(MAX_AGENT_ACTIVITY_DISPLAY_BYTES + 1), 5);
        assert!(budget.item_bytes.is_empty());
        assert_eq!(budget.total_display_bytes, 0);
    }

    #[test]
    fn shared_agent_activity_budget_cannot_be_reset_by_task_batches() {
        let mut window_budget = AgentActivityBudget::default();
        for _task_or_reopen in 0..32 {
            for _message in 0..64 {
                window_budget.push(8 * 1024);
            }
            assert!(window_budget.item_bytes.len() <= MAX_AGENT_ACTIVITY_CARDS);
            assert!(window_budget.total_display_bytes <= MAX_AGENT_ACTIVITY_DISPLAY_BYTES);
        }
        assert_eq!(window_budget.item_bytes.len(), MAX_AGENT_ACTIVITY_CARDS);
        assert_eq!(
            window_budget.total_display_bytes,
            MAX_AGENT_ACTIVITY_DISPLAY_BYTES
        );
    }

    #[test]
    fn agent_activity_accounts_the_exact_bounded_dynamic_labels() {
        let body = "界".repeat(MAX_AGENT_MESSAGE_DISPLAY_BYTES);
        let expected = "Shell Agent".len()
            + '\u{f007}'.len_utf8()
            + crate::review_input::safe_inline_display("Agent", 256).len()
            + agent_message_display_text(&body).len()
            + "Shell Agent activity: ".len()
            + crate::review_input::safe_inline_display("Agent", 256).len();
        assert_eq!(agent_message_display_bytes("Agent", &body), expected);
        assert!(agent_message_display_bytes("Agent", &body) < MAX_AGENT_ACTIVITY_DISPLAY_BYTES);
    }

    #[test]
    fn event_bridge_registry_claims_each_live_target_once() {
        let mut targets = Vec::new();
        let first = std::rc::Rc::new(());
        assert!(claim_weak_target(&mut targets, &first));
        assert!(!claim_weak_target(&mut targets, &first));
        assert_eq!(targets.len(), 1);

        drop(first);
        let replacement = std::rc::Rc::new(());
        assert!(claim_weak_target(&mut targets, &replacement));
        assert_eq!(
            targets.len(),
            1,
            "dead TermViews are purged before claiming"
        );
    }

    #[test]
    fn finished_block_consumes_the_approval_it_matches() {
        let mut slot = pending(7, "cat monitor_xilem_bar.sh");

        assert_eq!(
            take_pending_for_finished_block(&mut slot, "cat monitor_xilem_bar.sh", Some(1)),
            PendingCompletion::Matched(7)
        );
        assert!(slot.is_none(), "an approval is one-shot");
    }

    #[test]
    fn lost_execution_consumes_only_its_exact_generation() {
        let mut slot = pending(7, "printf safe");
        assert_eq!(take_pending_for_lost_execution(&mut slot, 2), None);
        assert!(slot.is_some());
        assert_eq!(take_pending_for_lost_execution(&mut slot, 1), Some(7));
        assert!(slot.is_none());
    }

    #[test]
    fn a_differing_capture_consumes_the_approval_and_cancels_the_session() {
        let mut slot = pending(7, "cat monitor_xilem_bar.sh");
        let mut session = crate::agent::AgentSession::new(2);
        session.submit_user("inspect the repository").unwrap();
        let proposal_id = match session
            .accept_model_reply(r#"{"action":"run","command":"cat monitor_xilem_bar.sh"}"#)
            .unwrap()
        {
            crate::agent::ModelOutcome::Proposal { id, .. } => id,
            other => panic!("expected proposal, got {other:?}"),
        };
        let _approved = session.approve(proposal_id).unwrap();
        assert!(matches!(
            session.state(),
            crate::agent::AgentState::AwaitingObservation { .. }
        ));

        // Fail closed: the model must not be told that some other command's
        // output is the result of the command the user approved, and the
        // protocol must not remain stranded waiting for an observation that
        // was deliberately discarded.
        assert_eq!(
            resolve_pending_for_finished_block(&mut session, &mut slot, "ls", Some(1)),
            PendingCompletion::CommandMismatch
        );
        assert!(slot.is_none(), "the approval is still consumed");
        assert_eq!(session.state(), crate::agent::AgentState::Cancelled);
    }

    #[test]
    fn identical_command_with_a_different_generation_is_not_observed() {
        let mut slot = pending(7, "printf same");

        assert_eq!(
            take_pending_for_finished_block(&mut slot, "printf same", Some(2)),
            PendingCompletion::Unrelated
        );
        assert!(
            slot.is_some(),
            "a stale completion must not consume the current approval"
        );
        assert_eq!(
            take_pending_for_finished_block(&mut slot, "printf same", Some(1)),
            PendingCompletion::Matched(7)
        );
        assert!(slot.is_none());
    }

    #[test]
    fn finished_block_without_an_approval_is_ignored() {
        let mut slot: Option<PendingExecution<u64>> = None;

        assert_eq!(
            take_pending_for_finished_block(&mut slot, "ls", None),
            PendingCompletion::Unrelated
        );
    }

    #[test]
    fn task_epoch_and_monotonic_id_reject_stale_callbacks_after_new_task() {
        let mut session = crate::agent::AgentSession::new(1);
        session.submit_user("old task").unwrap();
        let old_id = match session
            .accept_model_reply(r#"{"action":"run","command":"printf old"}"#)
            .unwrap()
        {
            crate::agent::ModelOutcome::Proposal { id, .. } => id,
            other => panic!("expected old proposal, got {other:?}"),
        };
        assert!(proposal_callback_is_current(
            true,
            0,
            0,
            session.state(),
            old_id
        ));

        session.reject(old_id).unwrap();
        session.start_new_task().unwrap();
        session.submit_user("new task").unwrap();
        let new_id = match session
            .accept_model_reply(r#"{"action":"run","command":"printf new"}"#)
            .unwrap()
        {
            crate::agent::ModelOutcome::Proposal { id, .. } => id,
            other => panic!("expected new proposal, got {other:?}"),
        };
        assert_ne!(old_id, new_id, "proposal ids remain process-monotonic");
        assert!(!proposal_callback_is_current(
            true,
            1,
            0,
            session.state(),
            new_id
        ));
        assert!(!proposal_callback_is_current(
            true,
            1,
            1,
            session.state(),
            old_id
        ));
        assert!(proposal_callback_is_current(
            true,
            1,
            1,
            session.state(),
            new_id
        ));
        assert!(!proposal_callback_is_current(
            false,
            1,
            1,
            session.state(),
            new_id
        ));
    }

    #[test]
    fn agent_message_display_is_bounded_and_neutralises_format_controls() {
        let body = format!(
            "line one\nline two \u{202e}\u{fff0}\u{e0080}{}",
            "界".repeat(MAX_AGENT_MESSAGE_DISPLAY_BYTES)
        );
        let displayed = agent_message_display_text(&body);

        assert!(displayed.len() <= MAX_AGENT_MESSAGE_DISPLAY_BYTES);
        assert!(displayed.contains("line one\nline two ���"));
        assert!(!displayed.contains('\u{202e}'));
        assert!(!displayed.contains('\u{fff0}'));
        assert!(!displayed.contains('\u{e0080}'));
    }

    #[test]
    fn agent_composer_input_is_bounded_on_a_utf8_boundary() {
        let bounded = bounded_agent_input("界".repeat(MAX_AGENT_INPUT_BYTES / "界".len() + 100));

        assert!(bounded.len() <= MAX_AGENT_INPUT_BYTES);
        assert!(bounded.chars().all(|ch| ch == '界'));
    }

    #[cfg(unix)]
    #[test]
    fn agent_snapshot_io_is_private_and_never_follows_links() {
        use std::os::unix::fs::{symlink, PermissionsExt};

        let root = std::env::temp_dir().join(format!(
            "forge-agent-snapshot-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&root).unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        let path = root.join("agent_session.json");
        let mut session = crate::agent::AgentSession::new(4);
        session.submit_user("inspect").unwrap();
        let snapshot = session.snapshot().unwrap();

        write_agent_snapshot_file(&path, &snapshot).unwrap();
        assert!(read_agent_snapshot_file(&path).unwrap().is_some());
        assert!(load_agent_snapshot(&path).is_some());
        assert!(!path.exists(), "a restored snapshot must be consumed once");

        let target = root.join("target");
        let linked = root.join("linked.json");
        std::fs::write(&target, snapshot.to_json().unwrap()).unwrap();
        symlink(&target, &linked).unwrap();
        assert!(read_agent_snapshot_file(&linked).is_err());
        assert_eq!(
            std::fs::read_to_string(target).unwrap(),
            snapshot.to_json().unwrap()
        );

        let malformed = root.join("malformed.json");
        std::fs::write(&malformed, b"not json").unwrap();
        std::fs::set_permissions(&malformed, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(load_agent_snapshot(&malformed).is_none());
        assert!(!malformed.exists());
        assert!(std::fs::read_dir(&root).unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with("malformed.json.corrupt-")
        }));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn concurrent_agent_snapshot_restore_has_exactly_one_winner() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        use std::sync::{Arc, Barrier};

        let root = std::env::temp_dir().join(format!(
            "forge-agent-snapshot-race-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&root).unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        let path = root.join("agent_session.json");
        let mut session = crate::agent::AgentSession::new(4);
        session.submit_user("inspect").unwrap();
        session
            .accept_model_reply(r#"{"action":"run","command":"ls"}"#)
            .unwrap();
        write_agent_snapshot_file(&path, &session.snapshot().unwrap()).unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().nlink(), 1);

        let barrier = Arc::new(Barrier::new(2));
        let workers: Vec<_> = (0..2)
            .map(|_| {
                let path = path.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    load_agent_snapshot(&path).is_some()
                })
            })
            .collect();
        let winners = workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .filter(|won| *won)
            .count();
        assert_eq!(winners, 1);
        assert!(!path.exists());
        assert!(!std::fs::read_dir(&root).unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .contains(".claim-")
        }));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn abandoned_agent_snapshot_claim_is_preserved_but_never_replayed() {
        use std::os::unix::fs::PermissionsExt;

        let root = std::env::temp_dir().join(format!(
            "forge-agent-snapshot-abandon-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&root).unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        let path = root.join("agent_session.json");
        let mut session = crate::agent::AgentSession::new(4);
        session.submit_user("inspect").unwrap();
        let snapshot = session.snapshot().unwrap();
        write_agent_snapshot_file(&path, &snapshot).unwrap();

        let claim = claim_agent_snapshot_file(&path).unwrap().unwrap();
        let claimed_path = claim.parent.join(&claim.claimed_name);
        drop(claim); // Simulate a process dying after rename and before decode.

        assert!(!path.exists());
        assert!(claimed_path.exists());
        assert_eq!(
            std::fs::read_to_string(&claimed_path).unwrap(),
            snapshot.to_json().unwrap()
        );
        assert!(load_agent_snapshot(&path).is_none());
        assert!(claimed_path.exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn semantically_invalid_claimed_snapshot_is_quarantined() {
        use std::os::unix::fs::PermissionsExt;

        let root = std::env::temp_dir().join(format!(
            "forge-agent-snapshot-invalid-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&root).unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        let path = root.join("agent_session.json");
        let mut session = crate::agent::AgentSession::new(4);
        session.submit_user("inspect").unwrap();
        session
            .accept_model_reply(r#"{"action":"run","command":"ls"}"#)
            .unwrap();
        let mut value: serde_json::Value =
            serde_json::from_str(&session.snapshot().unwrap().to_json().unwrap()).unwrap();
        let transcript = value["transcript"].as_array_mut().unwrap();
        let duplicate = transcript
            .iter()
            .find(|turn| turn.get("AssistantProposed").is_some())
            .unwrap()
            .clone();
        transcript.push(duplicate);
        std::fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();

        assert!(load_agent_snapshot(&path).is_none());
        assert!(!path.exists());
        assert!(std::fs::read_dir(&root).unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with("agent_session.json.corrupt-")
        }));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn duplicate_proposal_ids_in_a_snapshot_are_rejected() {
        let mut session = crate::agent::AgentSession::new(4);
        session.submit_user("inspect").unwrap();
        session
            .accept_model_reply(r#"{"action":"run","command":"ls"}"#)
            .unwrap();
        let snapshot = session.snapshot().unwrap();
        let mut exhausted: serde_json::Value =
            serde_json::from_str(&snapshot.to_json().unwrap()).unwrap();
        exhausted["next_proposal_id"] = serde_json::json!(u64::MAX);
        let exhausted = crate::agent::AgentSessionSnapshot::from_json(
            &serde_json::to_string(&exhausted).unwrap(),
        )
        .unwrap();
        assert!(restore_agent_snapshot(exhausted).is_err());

        let mut value: serde_json::Value =
            serde_json::from_str(&snapshot.to_json().unwrap()).unwrap();
        let transcript = value["transcript"].as_array_mut().unwrap();
        let duplicate = transcript
            .iter()
            .find(|turn| turn.get("AssistantProposed").is_some())
            .unwrap()
            .clone();
        transcript.push(duplicate);
        let malicious =
            crate::agent::AgentSessionSnapshot::from_json(&serde_json::to_string(&value).unwrap())
                .unwrap();

        assert!(restore_agent_snapshot(malicious).is_err());

        let mut value: serde_json::Value =
            serde_json::from_str(&snapshot.to_json().unwrap()).unwrap();
        let proposal = value["transcript"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find_map(|turn| turn.get_mut("AssistantProposed"))
            .unwrap();
        proposal["command"] = serde_json::json!("echo safe\u{202e}txt");
        let spoofed =
            crate::agent::AgentSessionSnapshot::from_json(&serde_json::to_string(&value).unwrap())
                .unwrap();
        assert!(restore_agent_snapshot(spoofed).is_err());
    }

    #[test]
    fn snapshot_cannot_rebind_approval_to_hidden_reordered_or_reused_ids() {
        let mut session = crate::agent::AgentSession::new(6);
        session.submit_user("inspect").unwrap();
        let first = match session
            .accept_model_reply(r#"{"action":"run","command":"printf first"}"#)
            .unwrap()
        {
            crate::agent::ModelOutcome::Proposal { id, .. } => id,
            other => panic!("expected first proposal, got {other:?}"),
        };
        session.reject(first).unwrap();
        let second = match session
            .accept_model_reply(r#"{"action":"run","command":"printf second"}"#)
            .unwrap()
        {
            crate::agent::ModelOutcome::Proposal { id, .. } => id,
            other => panic!("expected second proposal, got {other:?}"),
        };
        let base: serde_json::Value =
            serde_json::from_str(&session.snapshot().unwrap().to_json().unwrap()).unwrap();
        let decode = |value: serde_json::Value| {
            crate::agent::AgentSessionSnapshot::from_json(&serde_json::to_string(&value).unwrap())
                .unwrap()
        };
        assert!(restore_agent_snapshot(decode(base.clone())).is_ok());

        // A sole Pending status is insufficient: Forge only binds its visible
        // approval card to the final proposal/turn. Restoring an older pending
        // command behind a newer proposal would split reviewed UI identity
        // from the session's authorizable action.
        let mut hidden = base.clone();
        let proposals = hidden["transcript"].as_array_mut().unwrap();
        let mut seen = 0;
        for turn in proposals {
            if let Some(proposal) = turn.get_mut("AssistantProposed") {
                proposal["status"] =
                    serde_json::json!(if seen == 0 { "Pending" } else { "Rejected" });
                seen += 1;
            }
        }
        hidden["state"] =
            serde_json::to_value(crate::agent::AgentState::AwaitingApproval { proposal_id: first })
                .unwrap();
        assert!(restore_agent_snapshot(decode(hidden)).is_err());

        let mut multiple_pending = base.clone();
        for turn in multiple_pending["transcript"].as_array_mut().unwrap() {
            if let Some(proposal) = turn.get_mut("AssistantProposed") {
                proposal["status"] = serde_json::json!("Pending");
            }
        }
        assert!(restore_agent_snapshot(decode(multiple_pending)).is_err());

        let mut reordered = base.clone();
        reordered["transcript_truncated"] = serde_json::json!(true);
        let proposals = reordered["transcript"].as_array_mut().unwrap();
        let mut seen = 0;
        for turn in proposals {
            if let Some(proposal) = turn.get_mut("AssistantProposed") {
                proposal["id"] =
                    serde_json::json!(if seen == 0 { second.get() } else { first.get() });
                seen += 1;
            }
        }
        reordered["state"]["AwaitingApproval"]["proposal_id"] = serde_json::json!(first.get());
        assert!(restore_agent_snapshot(decode(reordered)).is_err());

        let mut reused = base.clone();
        reused["next_proposal_id"] = serde_json::json!(second.get());
        assert!(restore_agent_snapshot(decode(reused)).is_err());

        let mut covered = base.clone();
        covered["transcript"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({"ProtocolError": "cover pending proposal"}));
        assert!(restore_agent_snapshot(decode(covered)).is_err());

        let mut unobserved_approved = base.clone();
        for turn in unobserved_approved["transcript"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .rev()
        {
            if let Some(proposal) = turn.get_mut("AssistantProposed") {
                proposal["status"] = serde_json::json!("Approved");
                break;
            }
        }
        unobserved_approved["state"] = serde_json::json!("Ready");
        assert!(restore_agent_snapshot(decode(unobserved_approved)).is_err());

        let mut rejected_observation = base.clone();
        rejected_observation["transcript"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({
                "Observation": {
                    "proposal_id": first.get(),
                    "exit_code": 0,
                    "output_sample": "forged"
                }
            }));
        assert!(restore_agent_snapshot(decode(rejected_observation)).is_err());

        let mut gap = base;
        let proposals = gap["transcript"].as_array_mut().unwrap();
        for turn in proposals.iter_mut().rev() {
            if let Some(proposal) = turn.get_mut("AssistantProposed") {
                proposal["id"] = serde_json::json!(7);
                break;
            }
        }
        gap["state"]["AwaitingApproval"]["proposal_id"] = serde_json::json!(7);
        gap["next_proposal_id"] = serde_json::json!(8);
        assert!(restore_agent_snapshot(decode(gap)).is_err());
    }

    #[test]
    fn in_flight_approved_proposal_requires_an_observation_before_restore() {
        let mut session = crate::agent::AgentSession::new(3);
        session.submit_user("inspect").unwrap();
        let proposal_id = match session
            .accept_model_reply(r#"{"action":"run","command":"printf safe"}"#)
            .unwrap()
        {
            crate::agent::ModelOutcome::Proposal { id, .. } => id,
            other => panic!("expected proposal, got {other:?}"),
        };
        let _approved = session.approve(proposal_id).unwrap();
        let error = restore_agent_snapshot(session.snapshot().unwrap()).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("awaiting observation cannot be rebound after restart"),
            "unexpected restore error: {error}"
        );

        session.observe(proposal_id, 0, "safe output").unwrap();
        let observed = session.snapshot().unwrap().to_json().unwrap();
        assert!(restore_agent_snapshot(
            crate::agent::AgentSessionSnapshot::from_json(&observed).unwrap()
        )
        .is_ok());

        let mut wrong_state: serde_json::Value = serde_json::from_str(&observed).unwrap();
        wrong_state["state"] = serde_json::json!("Completed");
        let wrong_state = crate::agent::AgentSessionSnapshot::from_json(
            &serde_json::to_string(&wrong_state).unwrap(),
        )
        .unwrap();
        assert!(restore_agent_snapshot(wrong_state).is_err());

        let mut wrong_counter: serde_json::Value = serde_json::from_str(&observed).unwrap();
        wrong_counter["turns_used"] = serde_json::json!(0);
        let wrong_counter = crate::agent::AgentSessionSnapshot::from_json(
            &serde_json::to_string(&wrong_counter).unwrap(),
        )
        .unwrap();
        assert!(restore_agent_snapshot(wrong_counter).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn valid_in_flight_execution_checkpoint_is_retired_without_quarantine() {
        use std::os::unix::fs::PermissionsExt;

        let root = std::env::temp_dir().join(format!(
            "forge-agent-snapshot-in-flight-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&root).unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        let path = root.join("agent_session.json");

        let mut session = crate::agent::AgentSession::new(4);
        session.submit_user("run the approved check").unwrap();
        let proposal_id = match session
            .accept_model_reply(r#"{"action":"run","command":"printf safe"}"#)
            .unwrap()
        {
            crate::agent::ModelOutcome::Proposal { id, .. } => id,
            other => panic!("expected proposal, got {other:?}"),
        };
        let _approved = session.approve(proposal_id).unwrap();
        write_agent_snapshot_file(&path, &session.snapshot().unwrap()).unwrap();

        let restored = load_agent_snapshot(&path);
        assert!(
            restored.is_none(),
            "a restart must not restore a proposal whose execution identity was lost"
        );
        assert!(
            !path.exists(),
            "the public checkpoint must be consumed once"
        );
        let remaining = std::fs::read_dir(&root)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<Vec<_>>();
        assert!(
            remaining.is_empty(),
            "a valid but unresumable checkpoint must be retired, not quarantined: {remaining:?}"
        );

        // This is the same fallback used when building AgentRuntime.  It proves
        // the rejected checkpoint cannot leave the reopened card permanently
        // stuck in Running the approved command / AwaitingObservation.
        let reopened = restored.unwrap_or_else(|| crate::agent::AgentSession::new(4));
        assert_eq!(reopened.state(), crate::agent::AgentState::Ready);

        std::fs::remove_dir_all(root).unwrap();
    }
}
