#![allow(dead_code)]

pub mod agent {
    pub use jterm_core::agent::*;
}
pub mod ai;
pub mod block_view;
pub mod cli;
pub mod config;
pub mod config_store;
pub use jterm_core::{git_meta, notify, parser, review_input};
mod command_history {
    pub(crate) use jterm_core::command_history::*;
}

pub mod host {
    pub use jterm_core::host::*;

    pub const APP_ID: &str = "io.github.beamiter.jterm4";
}

pub mod child_env {
    pub use jterm_core::child_env::*;
}
pub mod execution_journal {
    pub use jterm_core::execution_journal::*;
}
pub mod keybindings;
pub mod logging;
pub mod notebook;
mod palette;
pub mod process {
    pub use jterm_core::process::*;
}
pub mod pty;
pub mod pty_input {
    pub use jterm_core::pty_input::*;
}
pub mod redact {
    pub use jterm_core::redact::*;
}
pub mod snapshot_file {
    pub use jterm_core::snapshot_file::*;
}
pub mod jsh_install {
    pub use jterm_core::jsh_install::*;
}
pub mod state;
pub mod terminal;
pub mod workflows;

#[path = "main.rs"]
pub mod app;
mod ui;
