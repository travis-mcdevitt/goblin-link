//! On-disk state of the Goblin Link daemon (std only, no async).
//!
//! Layout under the data directory:
//!
//! ```text
//! active                      name of the current profile (text, one line)
//! settings.json               device-wide settings
//! consent.json                device-wide command consent toggles
//! spool/                      files exchanged with clients (audio, images, drops)
//! profiles/<name>/
//!   identity.json pairing.json sdk_token      same files the Python SDK uses
//!   account.json                              account-mode credential blob (opaque)
//!   notifications.json commands.json          inbox and command log
//!   chats/                                    transcript cache and side-chat list
//! ```
//!
//! Secret files are written atomically with mode 0600 and never printed by `Debug`.
#![warn(missing_docs)]

pub mod chats;
pub mod commands;
mod error;
pub mod fsutil;
pub mod inbox;
pub mod profiles;
pub mod settings;
pub mod spool;

pub use profiles::{validate_profile_name, ProfileStore};

pub use error::StoreError;

use std::path::{Path, PathBuf};

/// Well-known paths below the data directory.
#[derive(Debug, Clone)]
pub struct Layout {
    root: PathBuf,
}

impl Layout {
    /// A layout rooted at `data_dir`. Nothing is created until a store opens.
    pub fn new(data_dir: impl Into<PathBuf>) -> Self {
        Self {
            root: data_dir.into(),
        }
    }

    /// The data directory itself.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// `<data>/profiles`.
    pub fn profiles_dir(&self) -> PathBuf {
        self.root.join("profiles")
    }

    /// `<data>/active`.
    pub fn active_file(&self) -> PathBuf {
        self.root.join("active")
    }

    /// `<data>/settings.json`.
    pub fn settings_file(&self) -> PathBuf {
        self.root.join("settings.json")
    }

    /// `<data>/consent.json`.
    pub fn consent_file(&self) -> PathBuf {
        self.root.join("consent.json")
    }

    /// `<data>/spool`.
    pub fn spool_dir(&self) -> PathBuf {
        self.root.join("spool")
    }
}
