//! Pure, conservative F02 dry-run decisions. No filesystem access or GC executor.
//!
//! Inputs are an explicit model of one namespace, not verified disk evidence.
//! A future collector must establish every documented precondition; existing
//! Forge snapshots do not establish this authority. See HISTORY_GC_PROTOCOL.md.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::os::unix::ffi::OsStrExt;

/// Retained directory identity; a pathname alone cannot bind evidence sources.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct NamespaceIdentity {
    pub device: u64,
    pub inode: u64,
}

/// Exact observation supplied by a future trusted collector. Digest is of the
/// complete bytes, never a filename or truncated restore result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FileIdentity {
    pub device: u64,
    pub inode: u64,
    pub bytes: u64,
    pub changed_ns: i128,
    pub digest: [u8; 32],
}

#[derive(Clone, Debug)]
pub(crate) struct OwnedFile {
    pub name: OsString,
    pub identity: FileIdentity,
}

#[derive(Clone, Debug)]
pub(crate) struct ObservedFile {
    pub name: OsString,
    /// Some only for a no-follow, current-user-owned, single-link regular file.
    /// Unsupported types, identity failures and unreadable files use None.
    pub identity: Option<FileIdentity>,
}

/// All names are exact basenames in the same validated, retained directory.
/// All collections must describe one coherent observation under a future
/// cross-process protocol. There is intentionally no collector in this stage.
pub(crate) struct Snapshot<'a> {
    pub ownership_namespace: Option<NamespaceIdentity>,
    pub inventory_namespace: NamespaceIdentity,
    /// Also binds live/pending reservations to this namespace.
    pub reference_namespace: Option<NamespaceIdentity>,
    pub manifest_version: Option<u32>,
    pub owned: &'a [OwnedFile],
    pub observed: &'a [ObservedFile],
    /// None means unknown/incomplete, not an empty set. Includes every durable
    /// restore/recovery source. Missing or corrupt sources cannot mean Some(&[]).
    pub references: Option<&'a [OsString]>,
    /// Live panes and all producer/publication/save reservations.
    pub live_or_pending: &'a [OsString],
    /// False on truncated/failed inventory or incomplete owner/live-pane/producer/
    /// queued-save enumeration. An unobserved live set is never known empty.
    pub inventory_complete: bool,
    /// Includes legacy/nonparticipating processes and unsupported reference state.
    pub legacy_or_unknown_references: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Blocker {
    NamespaceMismatch,
    MissingManifest,
    UnsupportedManifest,
    IncompleteReferences,
    IncompleteInventory,
    LegacyOrUnknownReferences,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PreserveReason {
    SnapshotBlocked,
    InvalidName,
    ConflictingRecords,
    AliasedIdentity,
    Unowned,
    MissingFile,
    UnsafeOrUnknownFile,
    IdentityChanged,
    Referenced,
    LiveOrPending,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Disposition {
    /// Conditional model candidate, never permission to move or delete a file.
    Candidate,
    Preserve(PreserveReason),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Decision {
    pub name: OsString,
    pub disposition: Disposition,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Plan {
    pub blockers: Vec<Blocker>,
    /// Exactly one decision for each distinct owned/observed basename, sorted
    /// by OsString ordering. Blockers have a fixed priority order.
    pub decisions: Vec<Decision>,
}

fn valid_basename(name: &std::ffi::OsStr) -> bool {
    let bytes = name.as_bytes();
    !bytes.is_empty()
        && bytes != b"."
        && bytes != b".."
        && !bytes.contains(&b'/')
        && !bytes.contains(&0)
}

pub(crate) fn plan(snapshot: Snapshot<'_>) -> Plan {
    let mut blockers = Vec::new();
    if snapshot.ownership_namespace != Some(snapshot.inventory_namespace)
        || snapshot.reference_namespace != Some(snapshot.inventory_namespace)
    {
        blockers.push(Blocker::NamespaceMismatch);
    }
    match snapshot.manifest_version {
        None => blockers.push(Blocker::MissingManifest),
        Some(1) => {}
        Some(_) => blockers.push(Blocker::UnsupportedManifest),
    }
    if snapshot
        .references
        .is_none_or(|names| names.iter().any(|name| !valid_basename(name)))
        || snapshot
            .live_or_pending
            .iter()
            .any(|name| !valid_basename(name))
    {
        blockers.push(Blocker::IncompleteReferences);
    }
    if !snapshot.inventory_complete {
        blockers.push(Blocker::IncompleteInventory);
    }
    if snapshot.legacy_or_unknown_references {
        blockers.push(Blocker::LegacyOrUnknownReferences);
    }

    // Count duplicates even when identical: ambiguity cannot grant authority.
    let mut owners = BTreeMap::new();
    for owner in snapshot.owned {
        let entry = owners
            .entry(&owner.name)
            .or_insert((0usize, &owner.identity));
        entry.0 += 1;
    }
    let mut files = BTreeMap::new();
    for file in snapshot.observed {
        let entry = files.entry(&file.name).or_insert((0usize, &file.identity));
        entry.0 += 1;
    }
    // A coherent single-link inventory cannot assign one physical file to
    // different names. Include ownership rows so stale/conflicting claims
    // cannot silently grant authority to another alias.
    let mut physical_names: BTreeMap<(u64, u64), BTreeSet<&OsString>> = BTreeMap::new();
    for owner in snapshot.owned {
        physical_names
            .entry((owner.identity.device, owner.identity.inode))
            .or_default()
            .insert(&owner.name);
    }
    for file in snapshot.observed {
        if let Some(identity) = &file.identity {
            physical_names
                .entry((identity.device, identity.inode))
                .or_default()
                .insert(&file.name);
        }
    }
    let aliased: BTreeSet<_> = physical_names
        .values()
        .filter(|names| names.len() > 1)
        .flat_map(|names| names.iter().copied())
        .collect();
    let names: BTreeSet<_> = owners.keys().chain(files.keys()).copied().collect();
    let references: BTreeSet<_> = snapshot.references.unwrap_or(&[]).iter().collect();
    let live: BTreeSet<_> = snapshot.live_or_pending.iter().collect();
    let decisions = names
        .into_iter()
        .map(|name| {
            let owner = owners.get(name);
            let file = files.get(name);
            let reason = if !blockers.is_empty() {
                Some(PreserveReason::SnapshotBlocked)
            } else if !valid_basename(name) {
                Some(PreserveReason::InvalidName)
            } else if owner.is_some_and(|(count, _)| *count != 1)
                || file.is_some_and(|(count, _)| *count != 1)
            {
                Some(PreserveReason::ConflictingRecords)
            } else if aliased.contains(name) {
                Some(PreserveReason::AliasedIdentity)
            } else {
                match (owner, file) {
                    (None, _) => Some(PreserveReason::Unowned),
                    (_, None) => Some(PreserveReason::MissingFile),
                    (Some(_), Some((_, None))) => Some(PreserveReason::UnsafeOrUnknownFile),
                    (Some((_, expected)), Some((_, Some(actual)))) if *expected != actual => {
                        Some(PreserveReason::IdentityChanged)
                    }
                    _ if references.contains(name) => Some(PreserveReason::Referenced),
                    _ if live.contains(name) => Some(PreserveReason::LiveOrPending),
                    _ => None,
                }
            };
            Decision {
                name: name.clone(),
                disposition: reason.map_or(Disposition::Candidate, Disposition::Preserve),
            }
        })
        .collect();
    Plan {
        blockers,
        decisions,
    }
}

#[cfg(test)]
#[path = "history_gc_tests.rs"]
mod tests;
