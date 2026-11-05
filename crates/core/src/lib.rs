//! Domain types and configuration shared across the music-hub backend.
//!
//! This crate performs no I/O beyond reading environment variables at startup;
//! everything here is pure and unit-testable so it carries the bulk of the
//! coverage that the CI gate requires.
#![forbid(unsafe_code)]

pub mod config;
pub mod ids;
pub mod model;
pub mod tls;

pub use config::{Config, ConfigError};
