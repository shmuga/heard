//! Heard: a desktop app for found sounds — trim, clean up, transform, tag, file into
//! collections. The sound sibling of Found (same stack, same patcher look).
#![allow(clippy::needless_range_loop)]

pub mod app;
pub mod audio;
pub mod graph;
pub mod io;
pub mod keys;
pub mod library;
pub mod ops;
pub mod player;
pub mod presets;
pub mod settings;
pub mod ui;
pub mod undo;
pub mod worker;

pub use app::{AppConfig, HeardApp};
