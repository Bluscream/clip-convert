//! Core logic for linux-clip-convert.
//!
//! This crate holds everything that can be reasoned about without a desktop:
//! configuration, the clipboard content model, the action model, and the
//! transformations the built-in actions perform. It has no UI dependencies, so
//! it builds and tests in seconds.

pub mod action;
pub mod auto;
pub mod clipboard;
pub mod config;
pub mod content;
pub mod encode;
pub mod exec;
pub mod image;
pub mod presets;
pub mod runner;
pub mod shorten;
pub mod text;
pub mod typing;
