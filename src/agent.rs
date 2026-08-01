//! Review-first agent core, shared with jsh through the `jagent` crate.
//!
//! The pure session state machine, action parser, safety heuristics, and
//! snapshot serialization live in `jagent` (sans-IO). This module re-exports
//! that surface under the historical `jterm_core::agent` paths and adds the
//! filesystem persistence helpers the jterm apps use for
//! `<config-dir>/<app>/agent_session.json`.

pub use jagent::safety::is_dangerous;
pub use jagent::session::{
    parse_action, sample_observation, AgentSession, AgentSessionSnapshot, AgentSnapshotError,
    AgentState, ApprovedCommand, CancellationToken, ModelOutcome, ParseError, ParsedAction,
    ProposalId, ProposalStatus, SessionError, Turn, MAX_AGENT_SNAPSHOT_JSON_BYTES,
};

/// Keep the historical family API fail-closed.
///
/// A string-only classifier cannot prove what a terminal's child shell will
/// actually execute: aliases and functions can replace an apparently harmless
/// program, repository configuration can make read-looking tools launch
/// helpers, and successful reads can expose sensitive data to the next model
/// turn. Frontends must therefore keep every proposal behind explicit review
/// until an integration-specific execution policy can validate the resolved
/// command and its data access.
pub fn is_auto_approvable(_command: &str) -> bool {
    false
}

/// Persist a snapshot to `path` with private (0600) permissions, creating
/// parent directories and replacing atomically via a sibling temp file.
pub fn write_snapshot_file(
    path: &std::path::Path,
    snapshot: &AgentSessionSnapshot,
) -> Result<(), AgentSnapshotError> {
    let encoded = snapshot.to_json()?;
    crate::snapshot_file::write_atomic_private(path, encoded.as_bytes())
        .map_err(|error| AgentSnapshotError::Encode(format!("write {}: {error}", path.display())))
}

/// Best-effort bounded read of a snapshot file. Any failure (missing file,
/// oversize, parse error) yields None — a broken snapshot must never block
/// opening a fresh session.
pub fn read_snapshot_file(path: &std::path::Path) -> Option<AgentSessionSnapshot> {
    let encoded =
        crate::snapshot_file::read_bounded(path, MAX_AGENT_SNAPSHOT_JSON_BYTES as u64).ok()?;
    AgentSessionSnapshot::from_json(&encoded).ok()
}

/// Remove a persisted snapshot; missing files are fine.
pub fn remove_snapshot_file(path: &std::path::Path) {
    let _ = std::fs::remove_file(path);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pending_snapshot() -> AgentSessionSnapshot {
        let mut session = AgentSession::new(10);
        session.submit_user("list files").unwrap();
        session.snapshot().unwrap()
    }

    struct TestDir(std::path::PathBuf);

    impl TestDir {
        fn new(label: &str) -> Self {
            static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "jterm-core-agent-{label}-{}-{id}",
                std::process::id()
            ));
            std::fs::create_dir_all(&path).unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
            }
            Self(path)
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn snapshot_files_round_trip_and_survive_bad_input() {
        let dir = TestDir::new("roundtrip");
        let path = dir.0.join("nested/agent_session.json");

        let snapshot = pending_snapshot();
        write_snapshot_file(&path, &snapshot).unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
            let parent_mode = std::fs::metadata(path.parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(parent_mode, 0o700);
        }

        let restored = read_snapshot_file(&path).expect("snapshot reads back");
        let restored = AgentSession::restore(restored).unwrap();
        let expected = AgentSession::restore(snapshot).unwrap();
        assert_eq!(restored.transcript(), expected.transcript());

        // Corrupt files read as None instead of failing the caller.
        std::fs::write(&path, "not json").unwrap();
        assert!(read_snapshot_file(&path).is_none());

        remove_snapshot_file(&path);
        assert!(read_snapshot_file(&path).is_none());
        // Removing a missing file is fine.
        remove_snapshot_file(&path);
    }

    #[test]
    fn oversized_and_corrupt_snapshots_still_fail_closed() {
        let dir = TestDir::new("invalid");
        let path = dir.0.join("agent_session.json");

        std::fs::write(&path, vec![b'x'; MAX_AGENT_SNAPSHOT_JSON_BYTES + 1]).unwrap();
        assert!(read_snapshot_file(&path).is_none());

        std::fs::write(&path, "not json").unwrap();
        assert!(read_snapshot_file(&path).is_none());
    }

    #[cfg(unix)]
    #[test]
    fn legacy_predictable_staging_symlink_is_never_followed() {
        use std::os::unix::fs::symlink;

        let dir = TestDir::new("legacy-staging-symlink");
        let parent = dir.0.join("nested");
        std::fs::create_dir(&parent).unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let path = parent.join("agent_session.json");
        let outside = dir.0.join("outside");
        std::fs::write(&outside, b"outside stays intact").unwrap();
        let legacy_staged = parent.join(format!(".agent_session.json.next.{}", std::process::id()));
        symlink(&outside, &legacy_staged).unwrap();

        write_snapshot_file(&path, &pending_snapshot()).unwrap();

        assert_eq!(std::fs::read(&outside).unwrap(), b"outside stays intact");
        assert!(std::fs::symlink_metadata(&legacy_staged)
            .unwrap()
            .file_type()
            .is_symlink());
        assert!(read_snapshot_file(&path).is_some());
    }

    #[cfg(unix)]
    #[test]
    fn destination_symlink_is_replaced_without_touching_its_target() {
        use std::os::unix::fs::symlink;

        let dir = TestDir::new("destination-symlink");
        let path = dir.0.join("agent_session.json");
        let outside = dir.0.join("outside");
        std::fs::write(&outside, b"outside stays intact").unwrap();
        symlink(&outside, &path).unwrap();

        write_snapshot_file(&path, &pending_snapshot()).unwrap();

        assert_eq!(std::fs::read(&outside).unwrap(), b"outside stays intact");
        assert!(std::fs::symlink_metadata(&path)
            .unwrap()
            .file_type()
            .is_file());
        assert!(read_snapshot_file(&path).is_some());
    }

    #[cfg(unix)]
    #[test]
    fn replacing_a_loose_existing_snapshot_restores_private_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let dir = TestDir::new("loose-target");
        let path = dir.0.join("agent_session.json");
        std::fs::write(&path, b"old").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();

        write_snapshot_file(&path, &pending_snapshot()).unwrap();

        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        assert!(read_snapshot_file(&path).is_some());
    }

    #[cfg(unix)]
    #[test]
    fn fifo_snapshot_read_returns_none_promptly() {
        let dir = TestDir::new("fifo");
        let path = dir.0.join("agent_session.json");
        let name = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
        // SAFETY: `name` is a NUL-terminated path that remains alive for the call.
        let made = unsafe { nix::libc::mkfifo(name.as_ptr(), 0o600) };
        if made != 0 {
            // Some sandboxes forbid FIFO creation; the shared snapshot module's
            // non-regular-file tests still cover the rejection in that case.
            return;
        }

        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        let reader_path = path.clone();
        let reader = std::thread::spawn(move || {
            let _ = sender.send(read_snapshot_file(&reader_path));
        });
        let result = receiver
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("FIFO read must not wait for a writer");
        assert!(result.is_none());
        reader.join().unwrap();
    }

    #[test]
    fn family_auto_approval_api_is_fail_closed() {
        for command in [
            "ls -la",
            "git status",
            "cat Cargo.toml",
            "hostname new-name",
            "tree -o /tmp/tree.txt",
        ] {
            assert!(
                !is_auto_approvable(command),
                "unexpected approval: {command}"
            );
        }
    }
}
