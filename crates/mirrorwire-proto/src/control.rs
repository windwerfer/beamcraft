use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

pub const MAX_CONTROL_MESSAGE: usize = 64 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Control {
    Hello {
        /// Must equal [`crate::PROTOCOL_VERSION`]; checked (with
        /// [`hello_protocol`]) before the rest of the message is parsed.
        protocol: u32,
        name: String,
        pairing_code: Option<String>,
    },
    Rejected {
        reason: String,
    },
    StreamInfo(StreamInfo),
    RequestKeyframe {
        reason: String,
    },
    Ping {
        t_viewer_us: u64,
    },
    Pong {
        t_viewer_us: u64,
        t_presenter_us: u64,
    },
    /// Viewer -> presenter, about twice a second: how the video packets of
    /// the last window arrived. Feeds the FEC overhead and the adaptive
    /// bitrate.
    LinkReport {
        /// Packet loss over the window, in 1/1000.
        loss_permille: u32,
        /// Packets the presenter sent for frames in the window.
        packets: u32,
        /// Data packets rebuilt from parity.
        recovered: u32,
        /// Frames that could not be rebuilt.
        unrecoverable: u32,
        /// One-way queueing delay of the window's frames; `None` when no
        /// frame arrived in the window.
        queue: Option<QueueDelay>,
    },
    Bye,
}

/// One-way queueing delay over a report window (µs above the lowest delay
/// of the last 30 s; [`crate::delay`]).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct QueueDelay {
    pub mean_us: u32,
    pub max_us: u32,
    /// Least-squares slope over the window (µs per second; positive = a
    /// queue is building up).
    pub trend_us_per_s: i32,
}

impl QueueDelay {
    pub fn mean_ms(&self) -> f32 {
        self.mean_us as f32 / 1000.0
    }

    pub fn max_ms(&self) -> f32 {
        self.max_us as f32 / 1000.0
    }

    pub fn trend_ms_per_s(&self) -> f32 {
        self.trend_us_per_s as f32 / 1000.0
    }
}

/// `queue 5 ms (max 12, -2 ms/s)`, as in the logs and the HUD.
impl std::fmt::Display for QueueDelay {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "queue {:.0} ms (max {:.0}, {:+.0} ms/s)",
            self.mean_ms(),
            self.max_ms(),
            self.trend_ms_per_s()
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StreamInfo {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub codec: String,
    pub encoder: String,
    pub bitrate_kbps: u32,
    /// The encoder heals losses by itself within about a second, so the viewer
    /// keeps decoding through a loss instead of waiting for a keyframe.
    pub intra_refresh: bool,
    /// How the cursor reaches the viewer. The presenter sends `stream_info`
    /// again when this changes.
    pub cursor: CursorMode,
    /// The presenter's forward error correction setting (`off`, `auto`,
    /// `20%`), for display.
    pub fec: String,
}

/// How the presenter delivers the mouse cursor.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum CursorMode {
    /// As pointer datagrams and image streams, drawn by the viewer
    /// ([`crate::cursor`]).
    Separate,
    /// Drawn into the video by the compositor.
    #[default]
    Embedded,
    /// Not shown.
    Hidden,
}

impl CursorMode {
    /// The name on the wire, in the status and in the HUD.
    pub fn as_str(self) -> &'static str {
        match self {
            CursorMode::Separate => "separate",
            CursorMode::Embedded => "embedded",
            CursorMode::Hidden => "hidden",
        }
    }
}

/// The `protocol` field of a `hello` body, read without parsing the rest, so
/// a peer of another version is rejected with a readable message even when
/// the message shapes differ. `None` if the body is not a `hello`.
pub fn hello_protocol(body: &[u8]) -> Option<u32> {
    #[derive(Deserialize)]
    struct Peek {
        #[serde(rename = "type")]
        kind: String,
        protocol: u32,
    }
    let p: Peek = serde_json::from_slice(body).ok()?;
    (p.kind == "hello").then_some(p.protocol)
}

impl Control {
    pub fn encode(&self) -> Vec<u8> {
        // Cannot fail for these plain types; the fallback keeps encode infallible.
        let body = serde_json::to_vec(self).unwrap_or_else(|_| br#"{"type":"bye"}"#.to_vec());
        let mut out = Vec::with_capacity(body.len() + 4);
        out.extend_from_slice(&(body.len() as u32).to_be_bytes());
        out.extend_from_slice(&body);
        out
    }

    pub fn decode_body(body: &[u8]) -> Result<Self> {
        Ok(serde_json::from_slice(body)?)
    }

    pub fn check_len(len: u32) -> Result<usize> {
        let len = len as usize;
        if len > MAX_CONTROL_MESSAGE {
            bail!("control message too large: {len} bytes");
        }
        Ok(len)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let msg = Control::Ping { t_viewer_us: 42 };
        let wire = msg.encode();
        let len = u32::from_be_bytes(wire[..4].try_into().unwrap());
        assert_eq!(Control::check_len(len).unwrap(), wire.len() - 4);
        assert_eq!(Control::decode_body(&wire[4..]).unwrap(), msg);
    }

    #[test]
    fn version_is_read_before_the_shape() {
        // A version-1 hello (with its `caps` list) still shows its version.
        let v1 = br#"{"type":"hello","protocol":1,"name":"imac","pairing_code":null,"caps":["fec-rs1"]}"#;
        assert_eq!(hello_protocol(v1), Some(1));
        // A future hello with a different shape, too.
        assert_eq!(
            hello_protocol(br#"{"type":"hello","protocol":9,"other":[1]}"#),
            Some(9)
        );
        assert_eq!(hello_protocol(br#"{"type":"bye"}"#), None);
        assert_eq!(hello_protocol(b"\xff garbage"), None);
        let hello = Control::Hello {
            protocol: crate::PROTOCOL_VERSION,
            name: "imac".into(),
            pairing_code: None,
        };
        assert_eq!(
            hello_protocol(&hello.encode()[4..]),
            Some(crate::PROTOCOL_VERSION)
        );
    }

    #[test]
    fn wire_names_are_pinned() {
        let info = Control::StreamInfo(StreamInfo {
            width: 1,
            height: 1,
            fps: 60,
            codec: "h264".into(),
            encoder: "x".into(),
            bitrate_kbps: 1,
            intra_refresh: false,
            cursor: CursorMode::Separate,
            fec: "auto".into(),
        });
        let body = info.encode();
        let text = std::str::from_utf8(&body[4..]).unwrap();
        assert!(
            text.contains(r#""cursor":"separate""#) && text.contains(r#""fec":"auto""#),
            "{text}"
        );
        assert_eq!(Control::decode_body(&body[4..]).unwrap(), info);

        let report = Control::LinkReport {
            loss_permille: 5,
            packets: 900,
            recovered: 2,
            unrecoverable: 0,
            queue: Some(QueueDelay {
                mean_us: 12_000,
                max_us: 30_000,
                trend_us_per_s: -4_500,
            }),
        };
        let body = report.encode();
        let text = std::str::from_utf8(&body[4..]).unwrap();
        assert!(
            text.contains(r#""queue":{"mean_us":12000,"max_us":30000,"trend_us_per_s":-4500}"#),
            "{text}"
        );
        assert_eq!(Control::decode_body(&body[4..]).unwrap(), report);
    }
}
