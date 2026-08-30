//! Host integration for native and Flatpak launches, and the family's
//! executable lookup.
//!
//! The lookup rules, the Flatpak bridge, and the bounded probe runner live in
//! [`jterm_core::host`], shared with the other terminals; this module is only
//! forge's surface for them. What stays local:
//!
//! - [`APP_ID`], the forge application id every GTK and identity consumer
//!   reads through `crate::host`.
//! - [`interactive_bash_path`], the shell-selection rule that needs the
//!   system's interactive bash rather than whichever bash an inherited PATH
//!   names first.
//! - `helper_command`, which core keeps crate-visible. It mirrors core's
//!   implementation — trusted canonical resolution and the `/usr/bin:/bin`
//!   child PATH clamp — so the remaining local caller (the CLI doctor) shares
//!   core's contract until it migrates too. The doctor's bounded probe itself
//!   runs through [`jterm_core::helper::bounded_command_output`]. Keep this in
//!   step with `jterm_core::host` when core's changes.
//!
//!   Command correction no longer routes through it. Its
//!   `writable_by_current_user` predicate trusts an executable owned by a
//!   *third* user and refuses every system helper when the terminal itself
//!   runs as root; `jterm_core::helper::trusted_component` is the corrected
//!   policy, and `jterm_core::command_correction` uses only that one. The
//!   doctor's probes are user-invoked and non-automatic, which is why this
//!   copy is still tolerable there — but it is the last caller, and closing
//!   it out is the remaining work.
//! - `HOST_HELPER_LAUNCHER`, the Flatpak host-side `PATH` re-clamp. The
//!   correction engine builds its own bridge argv (it will not accept a
//!   `Command` an app resolved by its own rules), so it names this script
//!   through `LocalEvidence::Bridged` rather than calling `helper_command`.
//!   One definition, two builders.

pub use jterm_core::host::*;

use std::ffi::OsStr;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

pub const APP_ID: &str = "io.github.beamiter.forge";

const MAX_HOST_COMMAND_NAME_BYTES: usize = 4 * 1024;
const TRUSTED_HELPER_PATH: &str = "/usr/bin:/bin";
// The launcher re-clamps PATH on the host side of the Flatpak bridge before
// exec, so host-side lookup also ignores empty or relative entries. Nothing
// else is preserved from the sandboxed environment: the jsh install check no
// longer runs through this launcher — it lives in `jterm_core::jsh_install`,
// which exports JSH_LOOKUP_PATH for the installer itself.
pub(crate) const HOST_HELPER_LAUNCHER: &str = r#"set -f
PATH=/usr/bin:/bin
export PATH
exec "$0" "$@"
"#;

/// The interactive-bash wrapper runs the *user's* rc, so it needs the system's
/// interactive bash — not whichever bash the PATH we inherited happens to name
/// first. A `nix develop`/`nix-shell` puts stdenv's bash ahead of the system
/// one, and that build has no programmable completion: no `complete` builtin,
/// and `progcomp`/`hostcomplete` are not shopt names. A stock `~/.bashrc`
/// sources `/usr/share/bash-completion/bash_completion`, so every one of its
/// directives fails and ~65 error lines land on the pane before the shell's
/// first prompt — where a continuous surface never clears them away.
pub fn interactive_bash_path() -> Option<PathBuf> {
    [
        "/usr/bin/bash",
        "/bin/bash",
        "/run/current-system/sw/bin/bash",
    ]
    .into_iter()
    .map(PathBuf::from)
    .find(|candidate| is_executable_file(candidate))
    .or_else(|| find_executable_in_path("bash"))
}

/// Resolve the sandbox-side bridge without ever consulting an empty or
/// relative PATH entry. The absolute fallback deliberately fails closed when
/// Flatpak support is unavailable instead of executing a project-local file
/// named `flatpak-spawn`.
fn flatpak_spawn_program() -> PathBuf {
    let conventional = PathBuf::from("/usr/bin/flatpak-spawn");
    if is_executable_file(&conventional) {
        conventional
    } else {
        find_executable_in_path("flatpak-spawn").unwrap_or(conventional)
    }
}

fn trusted_helper_program(flatpak: bool, name: &str, path: Option<&OsStr>) -> Option<PathBuf> {
    if name.is_empty()
        || name.len() > MAX_HOST_COMMAND_NAME_BYTES
        || name.contains('/')
        || name.contains('\0')
        || name.chars().any(char::is_control)
    {
        return None;
    }
    if flatpak {
        // An absolute path visible inside the sandbox need not exist on the
        // host. Keep host-side lookup for Flatpak; native launches can and must
        // resolve from absolute PATH entries before changing cwd.
        Some(PathBuf::from(name))
    } else {
        std::env::split_paths(path?).find_map(|directory| {
            if !directory.is_absolute() {
                return None;
            }
            trusted_system_executable(&directory.join(name))
        })
    }
}

/// Resolve an automatic helper to its canonical, non-user-writable target.
///
/// Delegates to [`jterm_core::helper::trusted_system_executable`] rather than
/// keeping a second predicate here. The local copy asked
/// `mode & 0o022 != 0 || (uid == euid && mode & 0o200 != 0)`, which called a
/// binary owned by a *third* user trusted — a `test` owned by another account,
/// mode 0755, ahead of `/usr/bin` on the doctor's PATH was resolved and
/// spawned — and called every root-owned system binary untrusted when forge
/// itself runs as root, disabling the doctor's probes in containers. Core's
/// predicate rejects `owner != 0 && owner != euid` and carves out euid 0
/// explicitly. The correction surface moved onto core this round; leaving a
/// weaker second copy behind for the CLI doctor to reuse is how the hole grows
/// back.
#[cfg(unix)]
fn trusted_system_executable(candidate: &Path) -> Option<PathBuf> {
    jterm_core::helper::trusted_system_executable(candidate)
}

#[cfg(not(unix))]
fn trusted_system_executable(candidate: &Path) -> Option<PathBuf> {
    // Automatic helper integrations are Unix-only today. Keep other targets
    // fail-closed until they have an equivalent ownership policy.
    let _ = candidate;
    None
}

/// Construct a command for an application-owned helper. Unlike [`command`],
/// native lookup ignores empty and relative PATH entries: opening a project
/// containing a file named `git`, `curl`, or `notify-send` must never turn a
/// background integration into repository-controlled code execution.
pub(crate) fn helper_command(name: &str) -> io::Result<Command> {
    helper_command_for(is_flatpak(), name, std::env::var_os("PATH").as_deref())
}

/// [`helper_command`] with the Flatpak/native decision and the lookup PATH
/// made explicit, mirroring core's internal constructor so tests can exercise
/// both branches without a sandbox.
fn helper_command_for(flatpak: bool, name: &str, path: Option<&OsStr>) -> io::Result<Command> {
    let program = trusted_helper_program(flatpak, name, path).ok_or_else(not_executable)?;
    if !flatpak {
        let mut command = command(program);
        command.env("PATH", TRUSTED_HELPER_PATH);
        return Ok(command);
    }

    // Resolve the helper in the host namespace, but filter empty and relative
    // PATH entries there before exec. A project-local `curl` or `git` must not
    // become trusted merely because the Flatpak bridge changed directory to
    // that project. `/bin/sh` is absolute and is only a small launcher whose
    // script uses shell builtins before replacing itself with the helper.
    let mut command = Command::new(flatpak_spawn_program());
    command.args(["--host", "--watch-bus"]);
    command
        .args(["/bin/sh", "-c", HOST_HELPER_LAUNCHER])
        .arg(program);
    command.env("PATH", TRUSTED_HELPER_PATH);
    Ok(command)
}

fn not_executable() -> io::Error {
    io::Error::new(
        io::ErrorKind::NotFound,
        "PTY executable does not exist or is not executable",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn implicit_helpers_cannot_be_hijacked_by_a_writable_path_entry() {
        let root =
            std::env::temp_dir().join(format!("forge-host-trusted-helper-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let program = root.join("curl");
        std::fs::write(&program, b"#!/bin/sh\n").unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let path = std::ffi::OsString::from(&root);

        // A user-writable directory is untrusted even though it is absolute
        // and the file inside it is executable.
        assert_eq!(trusted_helper_program(false, "curl", Some(&path)), None);
        assert_eq!(
            trusted_helper_program(false, "curl", Some(OsStr::new(":."))),
            None
        );
        // Host lookup happens in a different namespace, so Flatpak retains a
        // bare token for the bridge instead of reusing a sandbox path.
        assert_eq!(
            trusted_helper_program(true, "curl", Some(OsStr::new(":."))),
            Some(PathBuf::from("curl"))
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(unix)]
    #[test]
    fn native_helpers_use_a_fixed_child_path() {
        let Ok(command) = helper_command_for(false, "sh", Some(OsStr::new("/usr/bin:/bin"))) else {
            // Non-standard development hosts may not have a system-owned sh.
            return;
        };
        let program = Path::new(command.get_program());
        assert!(program.is_absolute());
        assert_eq!(std::fs::canonicalize(program).unwrap(), program);
        assert_eq!(command.get_args().count(), 0);
        let child_path = command
            .get_envs()
            .find_map(|(name, value)| (name == "PATH").then_some(value))
            .flatten();
        assert_eq!(child_path, Some(OsStr::new(TRUSTED_HELPER_PATH)));
    }

    #[test]
    fn flatpak_helpers_filter_the_host_path_before_exec() {
        let command = helper_command_for(true, "curl", Some(OsStr::new(":."))).unwrap();
        assert!(Path::new(command.get_program()).is_absolute());
        let arguments = command
            .get_args()
            .map(|value| value.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(arguments[0..2], ["--host", "--watch-bus"]);
        assert_eq!(arguments[2], "/bin/sh");
        assert_eq!(arguments[3], "-c");
        assert_eq!(arguments[4], HOST_HELPER_LAUNCHER);
        assert_eq!(arguments[5], "curl");
        let child_path = command
            .get_envs()
            .find_map(|(name, value)| (name == "PATH").then_some(value))
            .flatten();
        assert_eq!(child_path, Some(OsStr::new(TRUSTED_HELPER_PATH)));
    }
}
