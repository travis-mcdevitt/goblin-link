//! Device side of Goblin Link Bluetooth pairing.
//!
//! [`session`] is the v5 handshake and record encryption, [`framing`] the chunking of messages
//! over GATT, [`setup`] the commands the Muse app sends while it sets a device up, and [`ble`]
//! the BlueZ peripheral that carries them. A port of the Python SDK's `pairing.py`,
//! `ble_framing.py`, `ble_setup.py` and `ble_server_dbusnext.py`.
#![warn(missing_docs)]

pub mod ble;
pub mod framing;
pub mod session;
pub mod setup;
