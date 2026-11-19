//! Camera QR scan for the viewer's connect screen. On Linux (cargo feature `portal`) the
//! Camera portal + PipeWire feed frames to `pairing::scan_qr_luma`; elsewhere the button is
//! hidden (Android scans with the phone's camera app, which opens the `mirrorwire://` link).

use mirrorwire_net::pairing::ConnectInfo;

/// What a scan reports on one UI frame.
#[cfg_attr(not(all(target_os = "linux", feature = "portal")), allow(dead_code))]
pub enum Poll {
    /// Still running; show this status next to the button.
    Running(String),
    /// A `mirrorwire://` link was read.
    Found(ConnectInfo),
    /// Ended without a result; show this in the window.
    Failed(String),
}

#[cfg(all(target_os = "linux", feature = "portal"))]
mod imp {
    use super::*;
    use mirrorwire_media::source::camera::{CameraScanner, ScanEvent};

    pub const AVAILABLE: bool = true;

    pub struct Scan {
        scanner: CameraScanner<ConnectInfo>,
        status: String,
    }

    impl Scan {
        pub fn start() -> Self {
            Self {
                scanner: CameraScanner::start(mirrorwire_net::pairing::scan_qr_luma),
                status: "starting the camera...".into(),
            }
        }

        pub fn poll(&mut self) -> Poll {
            while let Some(ev) = self.scanner.poll() {
                match ev {
                    ScanEvent::Status(s) => self.status = s,
                    ScanEvent::Found(info) => return Poll::Found(info),
                    ScanEvent::Failed(e) => return Poll::Failed(format!("Camera scan: {e}")),
                }
            }
            Poll::Running(self.status.clone())
        }
    }
}

#[cfg(not(all(target_os = "linux", feature = "portal")))]
mod imp {
    use super::*;

    pub const AVAILABLE: bool = false;

    pub struct Scan;

    impl Scan {
        pub fn start() -> Self {
            Scan
        }

        pub fn poll(&mut self) -> Poll {
            Poll::Failed("Camera scan is not available in this build".into())
        }
    }
}

pub use imp::{Scan, AVAILABLE};
