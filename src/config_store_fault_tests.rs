//! Process-death tests execute the actual persistence functions, in owned fixtures.
//! SIGKILL tests process recovery, not loss of the kernel page cache/power failure.
use super::*;
use std::cell::RefCell;
use std::os::unix::fs::{DirBuilderExt, MetadataExt};
use std::process::{Child, Command, Stdio};

type Observer = Box<dyn Fn(&str, &Path)>;
thread_local! {
    static OBSERVER: RefCell<Option<Observer>> = RefCell::new(None);
    static IO_FAILURE: RefCell<Option<(String, PathBuf)>> = RefCell::new(None);
}
pub(super) fn io_fault(operation: &str, target: &Path) -> io::Result<()> {
    IO_FAILURE.with(|fault| {
        if fault
            .borrow()
            .as_ref()
            .is_some_and(|(op, path)| op == operation && path == target)
        {
            Err(io::Error::from_raw_os_error(nix::libc::EIO))
        } else {
            Ok(())
        }
    })
}
struct FailureGuard;
impl Drop for FailureGuard {
    fn drop(&mut self) {
        IO_FAILURE.with(|f| *f.borrow_mut() = None);
    }
}
fn fail(operation: &str, target: &Path) -> FailureGuard {
    IO_FAILURE.with(|f| *f.borrow_mut() = Some((operation.into(), target.into())));
    FailureGuard
}
pub(super) fn checkpoint(event: &str, target: &Path) {
    OBSERVER.with(|observer| {
        if let Some(observer) = observer.borrow().as_ref() {
            observer(event, target);
        }
    });
}
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "forge-config-fault-{}-{}",
            std::process::id(),
            UNIQUE_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
        Self(path)
    }
    fn path(&self) -> PathBuf {
        self.0.join("config.toml")
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
struct OwnedChild(Child);
impl Drop for OwnedChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn spawn(fixture: &Fixture, operation: &str, event: &str, target: &str) -> OwnedChild {
    OwnedChild(
        Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "config_store::fault_tests::config_fault_child",
                "--ignored",
                "--nocapture",
            ])
            .env("FORGE_CONFIG_FAULT_ROOT", &fixture.0)
            .env("FORGE_CONFIG_FAULT_OPERATION", operation)
            .env("FORGE_CONFIG_FAULT_EVENT", event)
            .env("FORGE_CONFIG_FAULT_TARGET", target)
            .stdin(Stdio::null())
            .spawn()
            .unwrap(),
    )
}
fn ready(f: &Fixture, child: &mut OwnedChild) {
    let start = Instant::now();
    while !f.0.join("ready").exists() {
        assert!(
            child.0.try_wait().unwrap().is_none(),
            "child exited before intended checkpoint"
        );
        assert!(
            start.elapsed() < Duration::from_secs(15),
            "checkpoint timeout"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}
fn success(child: &mut OwnedChild) {
    let start = Instant::now();
    loop {
        if let Some(status) = child.0.try_wait().unwrap() {
            assert!(status.success(), "child failed: {status}");
            break;
        }
        assert!(start.elapsed() < Duration::from_secs(15), "child timeout");
        std::thread::sleep(Duration::from_millis(5));
    }
}
const A: &[u8] = b"opacity = 0.1\n[future]\nlineage = 'A'\n";
const B: &[u8] = b"opacity = 0.2\n[future]\nlineage = 'B'\n";
const C: &[u8] = b"opacity = 0.3\n[future]\nlineage = 'C'\n";
const D: &[u8] = b"opacity = 0.4\n[future]\nlineage = 'D'\n";
const EVENTS: &[&str] = &[
    "created",
    "written",
    "file-synced",
    "before-rename",
    "renamed",
    "directory-synced",
];
fn seed(f: &Fixture) {
    atomic_replace(&f.path(), C).unwrap();
    atomic_replace(&backup_path_for(&f.path()), B).unwrap();
    atomic_replace(&secondary_backup_path_for(&f.path()), A).unwrap();
}
fn private_entries(f: &Fixture) {
    for entry in fs::read_dir(&f.0).unwrap() {
        let entry = entry.unwrap();
        if entry.file_name() == "ready" || entry.file_name() == "continue" {
            continue;
        }
        let metadata = entry.metadata().unwrap();
        assert!(metadata.is_file());
        assert_eq!(metadata.mode() & 0o777, 0o600, "{:?}", entry.path());
        assert_eq!(metadata.nlink(), 1);
    }
}
#[test]
#[ignore = "owned subprocess entry point"]
fn config_fault_child() {
    let root = PathBuf::from(std::env::var_os("FORGE_CONFIG_FAULT_ROOT").expect("child fixture"));
    let path = root.join("config.toml");
    let operation = std::env::var("FORGE_CONFIG_FAULT_OPERATION").unwrap();
    let event = std::env::var("FORGE_CONFIG_FAULT_EVENT").unwrap();
    let target = std::env::var("FORGE_CONFIG_FAULT_TARGET").unwrap();
    OBSERVER.with(|observer| {
        *observer.borrow_mut() = Some(Box::new(move |actual, path| {
            if actual == event && path.file_name().unwrap() == target.as_str() {
                fs::write(root.join("ready"), actual).unwrap();
                let start = Instant::now();
                while !root.join("continue").exists() {
                    assert!(
                        start.elapsed() < Duration::from_secs(15),
                        "parent never released checkpoint"
                    );
                    std::thread::sleep(Duration::from_millis(5));
                }
            }
        }))
    });
    if operation == "atomic" {
        atomic_replace(&path, D).unwrap();
        return;
    }
    // APP_ONLY_START
    let result = if operation.starts_with("restore") {
        restore_backup_to_path(&path).map(|(_, revision)| revision)
    } else {
        let mut config = Config::safe_defaults();
        config.window_opacity = 0.4;
        save_config_to_path(&path, &config, Some(&ConfigRevision::from_bytes(C)))
    };
    if operation.ends_with("unsafe") {
        assert!(
            matches!(result, Err(ConfigWriteError::Io(_))),
            "unsafe revision must be rejected: {result:?}"
        );
    } else if operation.ends_with("conflict") {
        assert!(
            matches!(result, Err(ConfigWriteError::Conflict { .. })),
            "expected explicit conflict: {result:?}"
        );
    } else {
        result.unwrap();
    }
    // APP_ONLY_END
}
#[test]
fn atomic_process_death_keeps_complete_private_old_or_new() {
    for event in EVENTS {
        let f = Fixture::new();
        seed(&f);
        let mut child = spawn(&f, "atomic", event, "config.toml");
        ready(&f, &mut child);
        child.0.kill().unwrap();
        let status = child.0.wait().unwrap();
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(status.signal(), Some(nix::libc::SIGKILL));
        let committed = matches!(*event, "renamed" | "directory-synced");
        assert_eq!(fs::read(f.path()).unwrap(), if committed { D } else { C });
        assert_eq!(fs::read(backup_path_for(&f.path())).unwrap(), B);
        assert_eq!(fs::read(secondary_backup_path_for(&f.path())).unwrap(), A);
        private_entries(&f);
        // Re-open through the production no-follow reader, not only std::fs.
        assert_eq!(
            read_optional(&f.path()).unwrap().unwrap(),
            if committed { D } else { C }
        );
    }
}
#[test]
fn atomic_syscall_failures_preserve_bytes_and_remove_staging() {
    for operation in ["write", "file-sync", "rename", "directory-sync"] {
        let f = Fixture::new();
        seed(&f);
        let failure = fail(operation, &f.path());
        assert!(matches!(
            atomic_replace(&f.path(), D),
            Err(ConfigWriteError::Io(_))
        ));
        drop(failure);
        // Directory fsync can fail after rename: replacement is visible but
        // durability was not acknowledged. All earlier failures retain C.
        assert_eq!(
            read_optional(&f.path()).unwrap().unwrap(),
            if operation == "directory-sync" { D } else { C }
        );
        assert_eq!(fs::read_dir(&f.0).unwrap().count(), 3);
        private_entries(&f);
        // The error must not leave a lock or poisoned path behind.
        atomic_replace(&f.path(), C).unwrap();
    }
}
// APP_TESTS_START
#[test]
fn save_and_restore_process_death_preserve_lineage() {
    for operation in ["save", "restore"] {
        let targets: &[&str] = if operation == "save" {
            &["config.toml", "config.toml.bak.1", "config.toml.bak"]
        } else {
            &["config.toml", "config.toml.before-restore"]
        };
        for target in targets {
            for event in EVENTS {
                let f = Fixture::new();
                seed(&f);
                let mut child = spawn(&f, operation, event, target);
                ready(&f, &mut child);
                child.0.kill().unwrap();
                let status = child.0.wait().unwrap();
                use std::os::unix::process::ExitStatusExt;
                assert_eq!(status.signal(), Some(nix::libc::SIGKILL));
                let main = read_optional(&f.path()).unwrap().unwrap();
                let table = parse_valid_table(&f.path(), &main).unwrap();
                let main_committed =
                    *target == "config.toml" && matches!(*event, "renamed" | "directory-synced");
                if operation == "restore" {
                    assert_eq!(main, if main_committed { B } else { C });
                    assert_eq!(fs::read(backup_path_for(&f.path())).unwrap(), B);
                    assert_eq!(fs::read(secondary_backup_path_for(&f.path())).unwrap(), A);
                    if f.path().with_extension("toml.before-restore").exists() {
                        assert_eq!(fs::read(before_restore_path_for(&f.path())).unwrap(), C);
                    }
                } else {
                    assert_eq!(table["future"]["lineage"].as_str(), Some("C"));
                    if main_committed {
                        assert_eq!(table["opacity"].as_float(), Some(0.4));
                    } else {
                        assert_eq!(main, C);
                    }
                    let primary = fs::read(backup_path_for(&f.path())).unwrap();
                    let secondary = fs::read(secondary_backup_path_for(&f.path())).unwrap();
                    assert!(primary == B || primary == C);
                    assert!(secondary == A || secondary == B);
                    if primary == C {
                        assert_eq!(secondary, B);
                    }
                    if main_committed {
                        assert_eq!(primary, C);
                        assert_eq!(secondary, B);
                    }
                }
                private_entries(&f);
                // A fresh process must recover through the real restore entry point.
                let expected_recovery = fs::read(backup_path_for(&f.path())).unwrap();
                let expected_lineage = parse_valid_table(&f.path(), &expected_recovery).unwrap()
                    ["future"]["lineage"]
                    .as_str()
                    .unwrap()
                    .to_owned();
                let mut recovery = spawn(&f, "restore", "never", "none");
                success(&mut recovery);
                let recovered = fs::read(f.path()).unwrap();
                assert_eq!(recovered, expected_recovery);
                assert_eq!(
                    parse_valid_table(&f.path(), &recovered).unwrap()["future"]["lineage"].as_str(),
                    Some(expected_lineage.as_str())
                );
            }
        }
    }
}
#[test]
fn external_edits_during_staging_and_backup_io_are_explicit_conflicts() {
    for (operation, target, event) in [
        ("save-conflict", "config.toml", "file-synced"),
        ("save-conflict", "config.toml.bak", "directory-synced"),
        ("restore-conflict", "config.toml", "file-synced"),
        (
            "restore-conflict",
            "config.toml.before-restore",
            "directory-synced",
        ),
    ] {
        let f = Fixture::new();
        seed(&f);
        let mut child = spawn(&f, operation, event, target);
        ready(&f, &mut child);
        // Non-cooperating editor: atomic replacement without the advisory lock.
        atomic_replace(&f.path(), D).unwrap();
        fs::write(f.0.join("continue"), b"go").unwrap();
        success(&mut child);
        assert_eq!(fs::read(f.path()).unwrap(), D);
        assert!(!fs::read_dir(&f.0).unwrap().any(|e| e
            .unwrap()
            .file_name()
            .to_string_lossy()
            .contains(".next.")));
    }
}
#[test]
fn save_restore_syscall_failures_never_report_success() {
    for restore in [false, true] {
        let targets: &[&str] = if restore {
            &["config.toml", "config.toml.before-restore"]
        } else {
            &["config.toml", "config.toml.bak", "config.toml.bak.1"]
        };
        for target in targets {
            for operation in ["write", "file-sync", "rename", "directory-sync"] {
                let f = Fixture::new();
                seed(&f);
                let failure = fail(operation, &f.0.join(target));
                let result = if restore {
                    restore_backup_to_path(&f.path()).map(|(_, rev)| rev)
                } else {
                    save_config_to_path(
                        &f.path(),
                        &Config::safe_defaults(),
                        Some(&ConfigRevision::from_bytes(C)),
                    )
                };
                assert!(
                    matches!(result, Err(ConfigWriteError::Io(_))),
                    "{target} {operation}: {result:?}"
                );
                drop(failure);
                let bytes = read_optional(&f.path()).unwrap().unwrap();
                let committed = *target == "config.toml" && operation == "directory-sync";
                if !committed {
                    assert_eq!(bytes, C);
                } else if restore {
                    assert_eq!(bytes, B);
                } else {
                    assert_eq!(
                        parse_valid_table(&f.path(), &bytes).unwrap()["future"]["lineage"].as_str(),
                        Some("C")
                    );
                }
                private_entries(&f);
                assert!(!fs::read_dir(&f.0).unwrap().any(|e| {
                    let name = e.unwrap().file_name();
                    let name = name.to_string_lossy();
                    name.contains(".next.") || name.contains(".tmp.") || name.contains(".restore.")
                }));
                restore_backup_to_path(&f.path()).unwrap();
            }
        }
    }
}
#[test]
fn symlink_swap_during_save_and_restore_never_writes_through() {
    for operation in ["save-unsafe", "restore-unsafe"] {
        let f = Fixture::new();
        seed(&f);
        let protected = f.0.join("protected.toml");
        atomic_replace(&protected, D).unwrap();
        let mut child = spawn(&f, operation, "file-synced", "config.toml");
        ready(&f, &mut child);
        fs::remove_file(f.path()).unwrap();
        std::os::unix::fs::symlink(&protected, f.path()).unwrap();
        fs::write(f.0.join("continue"), b"go").unwrap();
        success(&mut child);
        assert_eq!(fs::read(&protected).unwrap(), D);
        assert!(fs::symlink_metadata(f.path())
            .unwrap()
            .file_type()
            .is_symlink());
        assert!(!fs::read_dir(&f.0).unwrap().any(|e| {
            let name = e.unwrap().file_name();
            let name = name.to_string_lossy();
            name.contains(".next.") || name.contains(".restore.")
        }));
    }
}
// APP_TESTS_END
