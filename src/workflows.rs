//! workflows — user-saved parameterized command templates.
//!
//! A workflow is a small TOML file in `~/.config/forge/workflows/`
//! that names a reusable command with `{placeholder}` slots. The
//! Ctrl+Shift+M palette lists them; selecting one opens a dialog
//! asking for each placeholder's value, then writes the substituted
//! command into the live PTY (no auto-Enter — the user reviews and
//! presses Return).
//!
//! Format (one workflow per file):
//!
//! ```toml
//! name = "Deploy to staging"
//! description = "Push the current branch and trigger the staging deploy"
//! command = "git push origin {branch} && ssh staging 'deploy {branch} --env={env}'"
//!
//! [[args]]
//! name = "branch"
//! description = "Branch to deploy"
//! default = "main"
//!
//! [[args]]
//! name = "env"
//! description = "Target environment"
//! default = "staging"
//! ```
//!
//! Placeholder syntax is the simplest thing that survives shell quoting
//! and is unambiguous: `{name}`. We do NOT support `${name}` because
//! that collides with shell variable expansion (a perfectly valid
//! workflow template containing `${HOME}` would silently get mangled).

use serde::Deserialize;
use std::collections::HashSet;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

const MAX_WORKFLOW_FILE_BYTES: u64 = 256 * 1024;
const MAX_DIRECTORY_ENTRIES: usize = 4_096;
const MAX_WORKFLOW_FILES_PER_DIRECTORY: usize = 512;
const MAX_WORKFLOW_DIRECTORIES: usize = 64;
const MAX_WORKFLOWS: usize = 1_024;
const MAX_WORKFLOW_NAME_BYTES: usize = 256;
const MAX_WORKFLOW_DESCRIPTION_BYTES: usize = 4 * 1024;
const MAX_WORKFLOW_COMMAND_BYTES: usize = 64 * 1024;
const MAX_WORKFLOW_FIELD_BYTES: usize = 4 * 1024;
const MAX_WORKFLOW_TAGS: usize = 64;
const MAX_WORKFLOW_ARGS: usize = 64;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Workflow {
    pub name: String,
    pub description: String,
    pub command: String,
    /// Optional search/category labels used by the unified command palette.
    pub tags: Vec<String>,
    /// Optional shell hint retained for compatibility with shared workflow
    /// libraries. Commands are still inserted for review, not auto-executed.
    pub shell: Option<String>,
    pub args: Vec<WorkflowArg>,
    /// Absolute path the workflow was loaded from. Used so the palette
    /// can offer "open file" / "reveal in folder" actions later.
    pub source_path: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkflowArg {
    pub name: String,
    pub description: String,
    pub default: String,
}

/// `~/.config/forge/workflows/`. Created lazily on first save; we never
/// `mkdir -p` on read — a missing dir just means "no workflows yet".
pub fn workflows_dir() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let home = std::env::var_os("HOME").unwrap_or_default();
            PathBuf::from(home).join(".config")
        });
    base.join("forge").join("workflows")
}

/// Workflow search path in precedence order. User-authored workflows win over
/// additional, installed and source-tree examples with the same name.
pub fn workflow_dirs() -> Vec<PathBuf> {
    let mut dirs = vec![workflows_dir()];
    if let Some(extra) = std::env::var_os("FORGE_WORKFLOW_DIR") {
        dirs.extend(std::env::split_paths(&extra).take(MAX_WORKFLOW_DIRECTORIES));
    }
    dirs.push(gtk4::glib::user_data_dir().join("forge").join("workflows"));
    dirs.extend(
        gtk4::glib::system_data_dirs()
            .into_iter()
            .map(|dir| dir.join("forge").join("workflows")),
    );
    dirs.push(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("scripts")
            .join("workflows"),
    );
    let mut unique = Vec::new();
    let mut seen = HashSet::new();
    for dir in dirs.into_iter().take(MAX_WORKFLOW_DIRECTORIES) {
        if seen.insert(dir.clone()) {
            unique.push(dir);
        }
    }
    unique
}

/// Locate the installed or source-tree quick-start notebook.
pub fn welcome_notebook_path() -> Option<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(asset_dir) = std::env::var_os("FORGE_ASSET_DIR") {
        candidates.push(PathBuf::from(asset_dir).join("notebooks/welcome.jtnb.md"));
    }
    candidates.push(
        gtk4::glib::user_data_dir()
            .join("forge")
            .join("notebooks")
            .join("welcome.jtnb.md"),
    );
    candidates.extend(
        gtk4::glib::system_data_dirs()
            .into_iter()
            .map(|dir| dir.join("forge").join("notebooks").join("welcome.jtnb.md")),
    );
    candidates.push(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("scripts")
            .join("notebooks")
            .join("welcome.jtnb.md"),
    );
    candidates.into_iter().find(|path| path.is_file())
}

/// Read all TOML or YAML workflow files from `dir` and parse each as a Workflow.
/// Files that fail to parse are silently skipped — a malformed template
/// shouldn't kill the palette for every other one. Returns workflows
/// sorted by name for stable palette order.
pub fn load_all_from(dir: &Path) -> Vec<Workflow> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut paths: Vec<PathBuf> = entries
        .take(MAX_DIRECTORY_ENTRIES)
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| workflow_extension(path).is_some())
        .take(MAX_WORKFLOW_FILES_PER_DIRECTORY)
        .collect();
    paths.sort();
    let mut out = Vec::new();
    for path in paths {
        let Some(extension) = workflow_extension(&path) else {
            continue;
        };
        let Ok(contents) = read_bounded_workflow(&path) else {
            continue;
        };
        let workflow = match extension.as_str() {
            "yaml" | "yml" => parse_yaml_workflow(&contents, &path),
            _ => parse_workflow(&contents, &path),
        };
        if let Some(wf) = workflow {
            out.push(wf);
            if out.len() == MAX_WORKFLOW_FILES_PER_DIRECTORY {
                break;
            }
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// Load all configured sources, deduplicating by name. Earlier directories
/// have higher precedence so installed examples never shadow user files.
pub fn load_all() -> Vec<Workflow> {
    let mut out = Vec::new();
    let mut names = HashSet::new();
    'directories: for dir in workflow_dirs().into_iter().take(MAX_WORKFLOW_DIRECTORIES) {
        for workflow in load_all_from(&dir) {
            if names.insert(workflow.name.clone()) {
                out.push(workflow);
                if out.len() == MAX_WORKFLOWS {
                    break 'directories;
                }
            }
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

fn workflow_extension(path: &Path) -> Option<String> {
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    matches!(extension.as_str(), "toml" | "yaml" | "yml").then_some(extension)
}

fn read_bounded_workflow(path: &Path) -> Result<String, String> {
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(nix::libc::O_NONBLOCK | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW);
    }
    let file = options
        .open(path)
        .map_err(|error| format!("read: {error}"))?;
    let metadata = file
        .metadata()
        .map_err(|error| format!("inspect: {error}"))?;
    if !metadata.is_file() {
        return Err("source is not a regular file".to_string());
    }
    if metadata.len() > MAX_WORKFLOW_FILE_BYTES {
        return Err(format!(
            "source exceeds the {MAX_WORKFLOW_FILE_BYTES}-byte limit"
        ));
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(MAX_WORKFLOW_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("read: {error}"))?;
    if bytes.len() as u64 > MAX_WORKFLOW_FILE_BYTES {
        return Err(format!(
            "source exceeds the {MAX_WORKFLOW_FILE_BYTES}-byte limit"
        ));
    }
    String::from_utf8(bytes).map_err(|error| format!("source is not UTF-8: {error}"))
}

#[derive(Debug, Deserialize)]
struct YamlWorkflow {
    name: String,
    #[serde(default)]
    description: String,
    command: String,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    shell: Option<String>,
    #[serde(default)]
    args: Vec<YamlWorkflowArg>,
}

#[derive(Debug, Deserialize)]
struct YamlWorkflowArg {
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    default: Option<String>,
}

fn command_is_reviewable(command: &str, source_path: &Path) -> bool {
    let source_display =
        jterm_core::review_input::safe_inline_display(&source_path.to_string_lossy(), 2 * 1024);
    if command.len() > MAX_WORKFLOW_COMMAND_BYTES {
        log::warn!(
            "workflows: skipping {}: command exceeds {MAX_WORKFLOW_COMMAND_BYTES} bytes",
            source_display
        );
        return false;
    }
    match jterm_core::review_input::validate(command) {
        Ok(_) => true,
        Err(error) => {
            log::warn!(
                "workflows: skipping {}: command is unsafe for review-only insertion: {error}",
                source_display
            );
            false
        }
    }
}

fn validate_display_field(label: &str, value: &str, max_bytes: usize, allow_empty: bool) -> bool {
    let valid = (allow_empty || !value.trim().is_empty())
        && value.len() <= max_bytes
        && !value.chars().any(char::is_control)
        && !jterm_core::review_input::contains_visual_spoofing(value);
    if !valid {
        log::warn!("workflows: invalid {label}");
    }
    valid
}

fn validate_workflow(workflow: &Workflow) -> bool {
    if !validate_display_field("name", &workflow.name, MAX_WORKFLOW_NAME_BYTES, false)
        || !validate_display_field(
            "description",
            &workflow.description,
            MAX_WORKFLOW_DESCRIPTION_BYTES,
            true,
        )
        || !command_is_reviewable(&workflow.command, &workflow.source_path)
        || workflow.tags.len() > MAX_WORKFLOW_TAGS
        || workflow.args.len() > MAX_WORKFLOW_ARGS
    {
        return false;
    }
    if !workflow
        .tags
        .iter()
        .all(|tag| validate_display_field("tag", tag, MAX_WORKFLOW_FIELD_BYTES, false))
        || workflow.shell.as_ref().is_some_and(|shell| {
            !validate_display_field("shell", shell, MAX_WORKFLOW_FIELD_BYTES, false)
        })
    {
        return false;
    }
    let mut names = HashSet::new();
    workflow.args.iter().all(|argument| {
        validate_display_field(
            "argument name",
            &argument.name,
            MAX_WORKFLOW_FIELD_BYTES,
            false,
        ) && names.insert(argument.name.as_str())
            && validate_display_field(
                "argument description",
                &argument.description,
                MAX_WORKFLOW_DESCRIPTION_BYTES,
                true,
            )
            && argument.default.len() <= MAX_WORKFLOW_COMMAND_BYTES
            && !argument.default.chars().any(char::is_control)
            && !jterm_core::review_input::contains_visual_spoofing(&argument.default)
    })
}

fn parse_yaml_workflow(source: &str, source_path: &Path) -> Option<Workflow> {
    let raw: YamlWorkflow = match serde_yaml::from_str(source) {
        Ok(raw) => raw,
        Err(err) => {
            log::warn!(
                "workflows: skipping {}: {err}",
                jterm_core::review_input::safe_inline_display(
                    &source_path.to_string_lossy(),
                    2 * 1024
                )
            );
            return None;
        }
    };
    let workflow = Workflow {
        name: raw.name,
        description: raw.description,
        command: raw.command,
        tags: raw.tags,
        shell: raw.shell,
        args: raw
            .args
            .into_iter()
            .filter(|arg| !arg.name.trim().is_empty())
            .map(|arg| WorkflowArg {
                name: arg.name,
                description: arg.description,
                default: arg.default.unwrap_or_default(),
            })
            .collect(),
        source_path: source_path.to_path_buf(),
    };
    validate_workflow(&workflow).then_some(workflow)
}

fn parse_workflow(toml_src: &str, source_path: &Path) -> Option<Workflow> {
    let table: toml::Table = toml::from_str(toml_src).ok()?;
    let name = table.get("name")?.as_str()?.to_string();
    let command = table.get("command")?.as_str()?.to_string();
    let description = table
        .get("description")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let tags = table
        .get("tags")
        .and_then(toml::Value::as_array)
        .map(|tags| {
            tags.iter()
                .filter_map(toml::Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    let shell = table
        .get("shell")
        .and_then(toml::Value::as_str)
        .map(str::to_string);

    let mut args = Vec::new();
    if let Some(raw_args) = table.get("args").and_then(|v| v.as_array()) {
        for entry in raw_args {
            let t = match entry.as_table() {
                Some(t) => t,
                None => continue,
            };
            let Some(name) = t.get("name").and_then(|v| v.as_str()) else {
                continue;
            };
            let description = t
                .get("description")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let default = t
                .get("default")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            args.push(WorkflowArg {
                name: name.to_string(),
                description,
                default,
            });
        }
    }

    let workflow = Workflow {
        name,
        description,
        command,
        tags,
        shell,
        args,
        source_path: source_path.to_path_buf(),
    };
    validate_workflow(&workflow).then_some(workflow)
}

/// Substitute `{name}` placeholders in `template` with values from
/// `bindings`. Unknown placeholders are left as-is (so the user sees
/// them in the rendered command and can fix the typo). Escape `{{` and
/// `}}` for literal braces, mirroring `format!` semantics — workflows
/// occasionally need to emit JSON or shell brace expansions.
pub fn substitute(template: &str, bindings: &[(String, String)]) -> Result<String, String> {
    if template.len() > MAX_WORKFLOW_COMMAND_BYTES || bindings.len() > MAX_WORKFLOW_ARGS {
        return Err("workflow substitution input exceeds its limit".to_string());
    }
    for (name, value) in bindings {
        if !validate_display_field("binding name", name, MAX_WORKFLOW_FIELD_BYTES, false)
            || value.len() > MAX_WORKFLOW_COMMAND_BYTES
            || value.chars().any(char::is_control)
            || jterm_core::review_input::contains_visual_spoofing(value)
        {
            return Err(format!("workflow binding '{name}' is unsafe or oversized"));
        }
    }
    let mut out = String::with_capacity(template.len().min(MAX_WORKFLOW_COMMAND_BYTES));
    let bytes = template.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'{' {
            // Shared YAML workflows use mustache-style `{{name}}`. Preserve
            // the historical literal-brace escape when no such binding exists.
            if i + 1 < bytes.len() && bytes[i + 1] == b'{' {
                if let Some(close_rel) =
                    bytes[i + 2..].windows(2).position(|window| window == b"}}")
                {
                    let close = i + 2 + close_rel;
                    let name = &template[i + 2..close];
                    if let Some((_, value)) = bindings.iter().find(|(key, _)| key == name) {
                        push_rendered(&mut out, value)?;
                        i = close + 2;
                        continue;
                    }
                }
                push_rendered(&mut out, "{")?;
                i += 2;
                continue;
            }
            // Find the closing `}`.
            if let Some(close_rel) = bytes[i + 1..].iter().position(|&c| c == b'}') {
                let close = i + 1 + close_rel;
                let name = &template[i + 1..close];
                if let Some((_, v)) = bindings.iter().find(|(n, _)| n == name) {
                    push_rendered(&mut out, v)?;
                } else {
                    // Unknown placeholder — keep verbatim so the user notices.
                    push_rendered(&mut out, &template[i..=close])?;
                }
                i = close + 1;
                continue;
            }
            // Unterminated `{` — emit literally and move on.
            push_rendered(&mut out, "{")?;
            i += 1;
        } else if b == b'}' && i + 1 < bytes.len() && bytes[i + 1] == b'}' {
            push_rendered(&mut out, "}")?;
            i += 2;
        } else {
            // Multi-byte UTF-8: push the whole codepoint by reading char_indices.
            let rest = &template[i..];
            if let Some(c) = rest.chars().next() {
                let mut encoded = [0_u8; 4];
                push_rendered(&mut out, c.encode_utf8(&mut encoded))?;
                i += c.len_utf8();
            } else {
                break;
            }
        }
    }
    jterm_core::review_input::validate(&out)
        .map_err(|error| format!("rendered command is unsafe: {error}"))?;
    Ok(out)
}

fn push_rendered(output: &mut String, addition: &str) -> Result<(), String> {
    if output.len().saturating_add(addition.len()) > MAX_WORKFLOW_COMMAND_BYTES {
        return Err(format!(
            "rendered command exceeds {MAX_WORKFLOW_COMMAND_BYTES} bytes"
        ));
    }
    output.push_str(addition);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn parse_minimal_workflow() {
        let src = r#"
            name = "Echo"
            command = "echo hi"
        "#;
        let wf = parse_workflow(src, Path::new("/tmp/x.toml")).unwrap();
        assert_eq!(wf.name, "Echo");
        assert_eq!(wf.command, "echo hi");
        assert_eq!(wf.description, "");
        assert!(wf.tags.is_empty());
        assert!(wf.shell.is_none());
        assert!(wf.args.is_empty());
    }

    #[test]
    fn parse_workflow_with_args() {
        let src = r#"
            name = "Greet"
            description = "Say hello to someone"
            command = "echo hello {name}"

            [[args]]
            name = "name"
            description = "Who to greet"
            default = "world"
        "#;
        let wf = parse_workflow(src, Path::new("/tmp/x.toml")).unwrap();
        assert_eq!(wf.args.len(), 1);
        assert_eq!(wf.args[0].name, "name");
        assert_eq!(wf.args[0].default, "world");
    }

    #[test]
    fn parse_missing_required_fields_returns_none() {
        let src = r#"description = "no name or command""#;
        assert!(parse_workflow(src, Path::new("/tmp/x.toml")).is_none());
    }

    #[test]
    fn parses_shared_yaml_workflow_metadata() {
        let src = r#"
name: Deploy
description: Deploy a service
command: "deploy {{service}}"
tags: [ops, deploy]
shell: bash
args:
  - name: service
    description: Service name
    default: api
"#;
        let wf = parse_yaml_workflow(src, Path::new("/tmp/deploy.yaml")).unwrap();
        assert_eq!(wf.tags, vec!["ops", "deploy"]);
        assert_eq!(wf.shell.as_deref(), Some("bash"));
        assert_eq!(wf.args[0].default, "api");
        assert_eq!(
            substitute(&wf.command, &[("service".into(), "web".into())]).unwrap(),
            "deploy web"
        );
    }

    #[test]
    fn rejects_multiline_or_control_character_commands() {
        assert!(parse_yaml_workflow(
            "name: Unsafe\ncommand: |\n  echo one\n  echo two\n",
            Path::new("/tmp/unsafe.yaml")
        )
        .is_none());
        assert!(parse_workflow(
            "name = 'safe\u{202e}txt'\ncommand = 'echo safe'\n",
            Path::new("/tmp/visual.toml")
        )
        .is_none());
        assert!(parse_workflow(
            "name = 'Unsafe'\ncommand = \"echo one\\necho two\"\n",
            Path::new("/tmp/unsafe.toml")
        )
        .is_none());
        assert!(parse_workflow(
            "name = 'Unsafe'\ncommand = \"echo \\u001b[31mred\"\n",
            Path::new("/tmp/escape.toml")
        )
        .is_none());
    }

    #[test]
    fn substitute_replaces_named_placeholders() {
        let out = substitute(
            "deploy {env} {target}",
            &[
                ("env".into(), "prod".into()),
                ("target".into(), "api".into()),
            ],
        )
        .unwrap();
        assert_eq!(out, "deploy prod api");
    }

    #[test]
    fn substitute_leaves_unknown_placeholders_intact() {
        let out = substitute(
            "hi {name}, your role is {role}",
            &[("name".into(), "Bea".into())],
        )
        .unwrap();
        // {role} unresolved — keep it visible so the user sees the typo.
        assert_eq!(out, "hi Bea, your role is {role}");
    }

    #[test]
    fn substitute_double_brace_escape() {
        let out = substitute("shell brace expansion: {{a,b,c}}", &[]).unwrap();
        assert_eq!(out, "shell brace expansion: {a,b,c}");
    }

    #[test]
    fn substitute_no_braces_passthrough() {
        let s = "git status --porcelain";
        assert_eq!(substitute(s, &[]).unwrap(), s);
    }

    #[test]
    fn substitute_handles_utf8_around_braces() {
        let out = substitute("🚀 deploy {env} 完了", &[("env".into(), "prod".into())]).unwrap();
        assert_eq!(out, "🚀 deploy prod 完了");
    }

    #[test]
    fn substitute_rejects_binding_amplification_and_visual_spoofing() {
        let template = "{value}".repeat(128);
        let huge = "x".repeat(1_024);
        assert!(substitute(&template, &[("value".into(), huge)]).is_err());
        assert!(substitute(
            "echo {value}",
            &[("value".into(), "safe\u{202e}txt".into())]
        )
        .is_err());
    }

    #[test]
    fn load_all_from_skips_non_toml_and_malformed() {
        let dir = tempdir();
        // good
        write_file(
            &dir.path().join("a.toml"),
            "name = \"A\"\ncommand = \"echo a\"\n",
        );
        // good
        write_file(
            &dir.path().join("b.toml"),
            "name = \"B\"\ncommand = \"echo b\"\n",
        );
        // good YAML
        write_file(
            &dir.path().join("c.yaml"),
            "name: C\ncommand: echo c\ntags: [example]\n",
        );
        // wrong extension
        write_file(&dir.path().join("c.txt"), "not a workflow");
        // malformed toml
        write_file(&dir.path().join("d.toml"), "this is = not valid =");
        // missing required field
        write_file(&dir.path().join("e.toml"), "description = \"oops\"");

        let wfs = load_all_from(dir.path());
        assert_eq!(wfs.len(), 3);
        assert_eq!(wfs[0].name, "A");
        assert_eq!(wfs[1].name, "B");
        assert_eq!(wfs[2].name, "C");
    }

    #[test]
    fn load_all_from_missing_dir_returns_empty() {
        let wfs = load_all_from(Path::new("/nonexistent/forge/workflows/never"));
        assert!(wfs.is_empty());
    }

    #[test]
    fn workflow_reader_rejects_oversize_and_symlink_files() {
        let dir = tempdir();
        let oversized = dir.path().join("oversized.toml");
        write_file(
            &oversized,
            &format!(
                "name = 'large'\ncommand = 'echo safe'\n#{}",
                "x".repeat(MAX_WORKFLOW_FILE_BYTES as usize)
            ),
        );
        assert!(load_all_from(dir.path()).is_empty());

        fs::remove_file(&oversized).unwrap();
        let target = dir.path().join("target.txt");
        write_file(&target, "name = 'linked'\ncommand = 'echo unsafe'\n");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&target, dir.path().join("linked.toml")).unwrap();
        #[cfg(unix)]
        assert!(load_all_from(dir.path()).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn workflow_reader_rejects_fifo_without_blocking() {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;

        let dir = tempdir();
        let fifo = dir.path().join("blocked.toml");
        let path = CString::new(fifo.as_os_str().as_bytes()).unwrap();
        // SAFETY: path is a live NUL-terminated pathname and mode is valid.
        assert_eq!(unsafe { nix::libc::mkfifo(path.as_ptr(), 0o600) }, 0);
        let started = std::time::Instant::now();
        assert!(load_all_from(dir.path()).is_empty());
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
    }

    #[test]
    fn every_bundled_workflow_is_parseable_and_review_only() {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("scripts")
            .join("workflows");
        let candidate_count = fs::read_dir(&dir)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
            .filter(|entry| workflow_file_for_test(&entry.path()))
            .count();
        let workflows = load_all_from(&dir);
        assert_eq!(workflows.len(), candidate_count);
        assert!(workflows.len() >= 6);
        assert!(workflows
            .iter()
            .all(|workflow| jterm_core::review_input::validate(&workflow.command).is_ok()));
    }

    // ----- test helpers (no external `tempfile` dep) -----

    struct TmpDir(PathBuf);
    impl TmpDir {
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for TmpDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    fn tempdir() -> TmpDir {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let p = std::env::temp_dir().join(format!("forge-wf-test-{nanos}"));
        fs::create_dir_all(&p).unwrap();
        TmpDir(p)
    }
    fn write_file(p: &Path, contents: &str) {
        let mut f = fs::File::create(p).unwrap();
        f.write_all(contents.as_bytes()).unwrap();
    }

    fn workflow_file_for_test(path: &Path) -> bool {
        path.extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| {
                matches!(
                    extension.to_ascii_lowercase().as_str(),
                    "toml" | "yaml" | "yml"
                )
            })
    }
}
