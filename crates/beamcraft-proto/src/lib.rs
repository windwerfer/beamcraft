pub mod clock;
pub mod control;
pub mod cursor;
pub mod delay;
pub mod fec;
pub mod fragment;
pub mod rate;
pub mod stats;

/// Presenter and viewer must speak the same version (checked in `hello`).
/// Bumped on every incompatible change; there is no compatibility with
/// other versions (decisions.md D6).
pub const PROTOCOL_VERSION: u32 = 2;
pub const ALPN: &[u8] = b"beamcraft/1";
pub const MDNS_SERVICE: &str = "_beamcraft._udp.local.";
pub const DEFAULT_PORT: u16 = 47_800;
