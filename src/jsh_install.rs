//! Install and update surfaces for the companion shell, jsh.
//!
//! The decisions — the vendored installer script, the bounded update check,
//! and the prompt policy — live in [`jterm_core::jsh_install`], shared with
//! the other terminals; this module is only forge's re-export so callers keep
//! their `crate::jsh_install` paths. The UI surface itself is `src/ui/jsh.rs`.

pub use jterm_core::jsh_install::*;
