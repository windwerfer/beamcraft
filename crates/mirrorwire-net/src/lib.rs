// Parsers of network input live here: no panicking shortcuts outside tests (AGENTS.md §1.9).
#![cfg_attr(
    not(test),
    deny(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::panic
    )
)]

//! Networking for MirrorWire: QUIC transport, discovery, identity and pairing.
//! Parses input from the network, so it must stay free of `unsafe` (AGENTS.md §1.12).
#![forbid(unsafe_code)]

pub mod diag;
pub mod discovery;
pub mod firewall;
pub mod identity;
mod lan_cc;
pub mod pairing;
pub mod settings;
pub mod tls;
pub mod transport;

pub fn config_dir(override_dir: Option<&std::path::Path>) -> std::path::PathBuf {
    override_dir
        .map(std::path::Path::to_path_buf)
        .or_else(|| dirs::config_dir().map(|d| d.join("mirrorwire")))
        .unwrap_or_else(|| std::path::PathBuf::from(".mirrorwire"))
}
