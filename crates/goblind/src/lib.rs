//! The Goblin Link daemon as a library, so its pieces can be driven from tests.
//!
//! The `goblind` binary is a thin wrapper around these modules. See
//! `docs/daemon.md` for the architecture.
#![warn(missing_docs)]

pub mod asks;
pub mod avatar;
mod backend;
pub mod builds;
pub mod call;
pub mod config;
pub mod daemon;
pub mod exec;
mod hermes;
pub mod media;
mod muse;
pub mod pairing;
pub mod service;
pub mod tokenpage;
pub mod voice;
