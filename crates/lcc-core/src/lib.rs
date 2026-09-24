//! Core logic for linux-clip-convert.
//!
//! This crate holds everything that can be reasoned about without a desktop:
//! configuration, the clipboard content model, the action model, and the
//! transformations the built-in actions perform. It has no UI dependencies, so
//! it builds and tests in seconds.

pub mod action;
pub mod config;
pub mod content;
pub mod presets;
pub mod text;
