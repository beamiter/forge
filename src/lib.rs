#![allow(dead_code)]

pub mod agent;
pub mod ai;
pub mod block_view;
pub mod cli;
mod command_history;
pub mod config;
pub mod config_store;
pub use jterm_core::{git_meta, notify, parser, review_input};

pub mod host {
    pub use jterm_core::host::*;

    pub const APP_ID: &str = "io.github.beamiter.jterm4";
}

pub mod keybindings;
pub mod logging;
pub mod notebook;
mod palette;
pub mod pty;
pub mod redact;
pub mod state;
pub mod terminal;
pub mod workflows;

#[path = "main.rs"]
pub mod app;
mod ui;
