//! Platform adapters for Goblin Link.
//!
//! A platform is a directory `platforms/<id>/` holding a `platform.toml` (see [`config`]) and
//! small POSIX shell hooks (see [`hook`] for the calling contract). [`Platform`] loads one and
//! exposes each capability as a method that returns [`PlatformError::Unavailable`] when the file
//! does not declare it, so callers can hide the related controls instead of failing.

pub mod config;
pub mod detect;
pub mod error;
pub mod hook;
pub mod led;
mod platform;
pub mod power;
pub mod presence;
pub mod sysvol;

pub use config::PlatformConfig;
pub use error::{HookFailure, PlatformError, Result};
pub use led::{Cycles, LedEffect, LedSetting, LedSnapshot, Leds, Rgb};
pub use platform::{Capabilities, EntryKind, MicGainGuard, Platform};
pub use power::{BatteryStatus, DeviceHealth};
pub use presence::{ConnectedStyle, PresenceLeds, PresenceOptions, PresenceState};
