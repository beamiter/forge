//! forge's workflow library — the app-local half.
//!
//! The engine is [`jterm_core::workflows`], the union of the four terminals'
//! formerly duplicated copies. Discovery, the bounded reader, both parsers,
//! validation, the template engine and the parameter-fill model moved there;
//! what is left here is the four decisions that are genuinely forge's — which
//! directory segment it looks under, which environment variable adds to the
//! search path, which XDG backend answers those questions, and which order the
//! library is listed in — plus [`welcome_notebook_path`], which lives in this
//! file only because it reuses the directory-search shape.
//!
//! forge's private copy was the outlier on nearly every rule, and adopting the
//! union is not a no-op. Four things change for a forge user:
//!
//! - **Type-wrong TOML now rejects the file.** forge hand-rolled its parser
//!   over `toml::Table` with `as_str().unwrap_or("")`, so `default = 3000` —
//!   an unquoted port, the most natural authoring mistake there is — silently
//!   became the empty string and the file *loaded*. The user got a blank Port
//!   field and, on Insert, `lsof -ti tcp: | xargs -r kill -TERM` at their
//!   prompt. Both formats go through serde derive now, so that file, a
//!   `tags = ["net", 1]`, and an `[[args]]` entry with no `name` all reject
//!   with a message naming the problem. This is a visible library shrink on
//!   upgrade and it is the correct direction: the other three already refused
//!   all three files.
//! - **Zero-argument workflows render.** forge wrote `workflow.command`
//!   straight to the pane whenever `args` was empty, at both of its activation
//!   sites, so its own documented `{{ }}` literal-brace escape was not applied
//!   there and the template never crossed validation on that path. There is
//!   one insertion path now — [`ArgsForm::render`] — and it does not care how
//!   many arguments a workflow declares.
//! - **Errors reach the log.** forge built a good error string in its bounded
//!   reader and then dropped it with `let Ok(x) = .. else { continue }`, so an
//!   oversized, symlinked or non-UTF-8 file vanished from the palette with no
//!   log line at all. That silence is why forge's other divergences went
//!   unnoticed for as long as they did.
//! - **No CWD-relative user tier.** [`user_workflow_dir`] used to be derived
//!   from `HOME` with `unwrap_or_default()`, so with `HOME` unset forge
//!   scanned `./.config/forge/workflows`: clone a repository containing that
//!   directory, start forge inside it, and its files became the
//!   *highest-precedence* workflows. A non-absolute answer is a failed lookup
//!   here, and a failed lookup is a skipped tier.
//!
//! And one change forge shares with the whole family: an argument that
//! declares no default and is left blank is now reported as a missing value
//! rather than substituted as the empty string. `kill -9 {pid}` with an
//! untouched Pid field no longer inserts `kill -9 `. See
//! `jterm_core::workflows`' module docs for why every UI in the family
//! defeated that guard.

use std::path::PathBuf;

use jterm_core::workflows::{search_path, DirSources, SearchPathSpec};

pub use jterm_core::workflows::{
    is_workflow_file, load_one, render, validate, workflow_files_in, ArgsForm, LoadOrder,
    PickerPolicy, Workflow, WorkflowArg, WorkflowPicker, MAX_LOGGED_PATH_BYTES,
};

/// The directory segment forge looks under, and — through
/// [`SearchPathSpec::for_app`] — the `FORGE_WORKFLOW_DIR` override derived
/// from it. Deriving rather than spelling both out is what stops one app from
/// reading its own directory while honouring another's override.
const APP: &str = "forge";

/// forge lists its palette alphabetically.
///
/// [`LoadOrder`] has no `Default`, deliberately: anvil and frost list in
/// directory-precedence order so the user's own files head the list, and this
/// was expressed in all four copies as the presence or absence of a single
/// `sort_by` line. Stating it here is the whole point — the ordering forge's
/// users have muscle memory for is a choice, not an accident of which sibling
/// the code was copied from.
const LOAD_ORDER: LoadOrder = LoadOrder::ByName;

/// glib's XDG lookups, which are not the `dirs` crate's.
///
/// anvil and forge ask glib; ember and frost ask `dirs`. They agree on a
/// normal Linux desktop and differ at exactly the edges that matter, so the
/// backend is injected rather than decided in core — hardcoding either one
/// would silently change which directories two of the four apps read, with
/// nothing in the diff to explain it. glib's lookups never fail, so all three
/// methods here answer; [`search_path`] still drops any non-absolute answer.
struct GlibDirs;

impl DirSources for GlibDirs {
    fn user_config_dir(&self) -> Option<PathBuf> {
        Some(gtk4::glib::user_config_dir())
    }

    fn user_data_dir(&self) -> Option<PathBuf> {
        Some(gtk4::glib::user_data_dir())
    }

    fn system_data_dirs(&self) -> Vec<PathBuf> {
        gtk4::glib::system_data_dirs()
    }
}

/// The source-tree tier, passed in rather than computed in core:
/// `env!("CARGO_MANIFEST_DIR")` is resolved at compile time against the crate
/// being compiled, so evaluating it inside `jterm_core` would point all four
/// apps at `jterm_core/scripts/workflows` — and their bundled-library contract
/// tests would keep passing, because they would then be asserting about a
/// directory that does not exist.
fn dev_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("scripts")
        .join("workflows")
}

/// forge's half of the search path, stated in one place.
///
/// [`SearchPathSpec::for_current_app`] is deliberately not used: it reads the
/// process identity, which answers with the neutral `"jterm"` before
/// `identity::init` and in every test binary, and a spec built on that reads
/// `~/.config/jterm/workflows` with no error anywhere.
fn spec() -> SearchPathSpec {
    SearchPathSpec::for_app(APP, Some(dev_root()))
}

/// `<user config>/forge/workflows/` — the tier the "no workflows yet" hint
/// points at. Created lazily on first save; a missing directory just means
/// "no workflows yet", so nothing here ever `mkdir -p`s on read.
///
/// `None` when no *absolute* user config directory can be resolved, which is
/// exactly when [`workflow_dirs`] drops that tier: the hint must not name a
/// directory the loader would refuse to read.
pub fn user_workflow_dir() -> Option<PathBuf> {
    let base = gtk4::glib::user_config_dir();
    base.is_absolute().then(|| base.join(APP).join("workflows"))
}

/// The workflow search path in precedence order: user config,
/// `$FORGE_WORKFLOW_DIR`, user data, each system data directory, then the
/// source tree. User-authored workflows win over installed and bundled
/// examples of the same name.
pub fn workflow_dirs() -> Vec<PathBuf> {
    search_path(&spec(), &GlibDirs)
}

/// The whole library, deduplicated by name with the earlier directory winning,
/// listed in `LOAD_ORDER`.
pub fn load_all() -> Vec<Workflow> {
    jterm_core::workflows::load_all(&workflow_dirs(), LOAD_ORDER)
}

/// Locate the installed or source-tree quick-start notebook.
///
/// Deliberately did **not** migrate to core: it is an asset lookup that lives
/// in this file only because it reuses the directory-search shape, and ember
/// and frost each documented not porting it because they have no notebook
/// surface at all.
pub fn welcome_notebook_path() -> Option<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(asset_dir) = std::env::var_os("FORGE_ASSET_DIR") {
        candidates.push(PathBuf::from(asset_dir).join("notebooks/welcome.jtnb.md"));
    }
    candidates.push(
        gtk4::glib::user_data_dir()
            .join(APP)
            .join("notebooks")
            .join("welcome.jtnb.md"),
    );
    candidates.extend(
        gtk4::glib::system_data_dirs()
            .into_iter()
            .map(|dir| dir.join(APP).join("notebooks").join("welcome.jtnb.md")),
    );
    candidates.push(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("scripts")
            .join("notebooks")
            .join("welcome.jtnb.md"),
    );
    candidates.into_iter().find(|path| path.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The engine's own behaviour — parsing, validation, the bounded reader,
    /// the template rules — is covered by `jterm_core::workflows`. What forge
    /// owns is the policy, so this pins every value the shim supplies.
    #[test]
    fn forge_pins_its_segment_its_override_variable_and_its_dev_root() {
        let spec = spec();
        assert_eq!(spec.app(), "forge");
        assert_eq!(spec.env_var(), "FORGE_WORKFLOW_DIR");
        assert_eq!(spec.dev_root(), Some(dev_root().as_path()));
        assert!(dev_root().ends_with("scripts/workflows"));
        // The manifest dir is this crate's, not jterm_core's — the whole
        // reason the tier is a parameter.
        assert!(dev_root().starts_with(env!("CARGO_MANIFEST_DIR")));
    }

    /// The spec's own tiers, with the machine's answers stubbed out so the
    /// assertion is about forge's policy rather than about whatever
    /// `XDG_DATA_DIRS` the test host happens to export.
    #[test]
    fn the_spec_puts_the_user_tier_first_and_the_source_tree_last() {
        struct OnlyConfig;
        impl DirSources for OnlyConfig {
            fn user_config_dir(&self) -> Option<PathBuf> {
                Some(PathBuf::from("/stub/config"))
            }
            fn user_data_dir(&self) -> Option<PathBuf> {
                None
            }
            fn system_data_dirs(&self) -> Vec<PathBuf> {
                Vec::new()
            }
        }

        let dirs = search_path(&spec(), &OnlyConfig);
        // `$FORGE_WORKFLOW_DIR` may legitimately add tiers between these two
        // on a developer's machine, so only the ends are pinned.
        assert_eq!(
            dirs.first(),
            Some(&PathBuf::from("/stub/config/forge/workflows"))
        );
        assert_eq!(dirs.last(), Some(&dev_root()));
    }

    /// The glib backend itself. anvil and forge ask glib precisely because
    /// `dirs::config_dir()` is not the same lookup, and glib's answers are the
    /// ones forge's users have their libraries under today.
    ///
    /// The source-tree tier is deliberately *not* asserted here: a host whose
    /// `XDG_DATA_DIRS` lists more than sixty-four entries — a Nix devshell
    /// does — fills the search path before the dev root is reached, which is
    /// the documented cap doing its job rather than a policy change.
    #[test]
    fn the_glib_backend_answers_with_absolute_directories() {
        let dirs = workflow_dirs();
        let expected_user = user_workflow_dir().expect("glib always resolves a config dir");
        assert_eq!(dirs.first(), Some(&expected_user));
        assert!(
            dirs.iter().all(|dir| dir.is_absolute()),
            "a relative tier is how forge came to scan ./.config/forge/workflows"
        );
        assert!(dirs.len() <= jterm_core::workflows::MAX_WORKFLOW_DIRECTORIES);
    }

    /// Every bundled example must still load under the rules forge now
    /// applies, and must still be safe to insert for review. `candidates ==
    /// loaded` is the load-bearing half: it fails if a shipped file stops
    /// parsing, which is exactly what the stricter serde path could have
    /// broken.
    #[test]
    fn every_bundled_workflow_is_parseable_and_review_only() {
        let dir = dev_root();
        let candidates = workflow_files_in(&dir);
        let workflows = jterm_core::workflows::load_all(std::slice::from_ref(&dir), LOAD_ORDER);
        assert_eq!(workflows.len(), candidates.len());
        assert!(workflows.len() >= 6);
        assert!(workflows
            .iter()
            .all(|workflow| jterm_core::review_input::validate(&workflow.command).is_ok()));

        // The pinned order, observed rather than assumed.
        let names: Vec<&str> = workflows.iter().map(|w| w.name.as_str()).collect();
        let mut sorted = names.clone();
        sorted.sort_unstable();
        assert_eq!(names, sorted, "forge pins LoadOrder::ByName");
    }

    /// Every bundled workflow is rendered by the same path the palette uses,
    /// with nothing typed. A file that declares a default for each argument
    /// must produce a command; one that leaves an argument undeclared must say
    /// so rather than insert a blank. This is the family-wide unfilled-argument
    /// fix observed through forge's own shipped library.
    #[test]
    fn a_bundled_workflow_renders_from_its_declared_defaults_alone() {
        let dir = dev_root();
        for workflow in jterm_core::workflows::load_all(std::slice::from_ref(&dir), LOAD_ORDER) {
            let name = workflow.name.clone();
            let form = ArgsForm::new(workflow);
            match form.render() {
                Ok(rendered) => assert!(!rendered.trim().is_empty(), "{name} rendered to nothing"),
                Err(error) => assert!(
                    error.contains("missing values:"),
                    "{name} failed for an unexpected reason: {error}"
                ),
            }
        }
    }
}
