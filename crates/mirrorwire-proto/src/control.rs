use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

pub const MAX_CONTROL_MESSAGE: usize = 64 * 1024;

/// The viewer's display, sent in `hello` (protocol 8).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct DisplayInfo {
    /// Physical pixels.
    pub width: u32,
    pub height: u32,
    /// Device scale factor in 1/1000 (2000 = 2x).
    pub scale_milli: u32,
}

impl DisplayInfo {
    pub const MAX_SIDE: u32 = 16_384;

    /// Clamp a size from the outside into a sane one; `None` if it is empty
    /// or absurd (hostile or broken viewers must not size a monitor).
    pub fn sanitized(self) -> Option<Self> {
        let ok = |v: u32| (1..=Self::MAX_SIDE).contains(&v);
        (ok(self.width) && ok(self.height) && (250..=8000).contains(&self.scale_milli))
            .then_some(self)
    }

    /// From a size in points and the points-to-pixels factor (egui).
    pub fn from_points(width: f32, height: f32, pixels_per_point: f32) -> Self {
        let px = |v: f32| {
            (v * pixels_per_point)
                .round()
                .clamp(1.0, Self::MAX_SIDE as f32) as u32
        };
        Self {
            width: px(width),
            height: px(height),
            scale_milli: (pixels_per_point * 1000.0).round().clamp(250.0, 8000.0) as u32,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Control {
    Hello {
        /// Must equal [`crate::PROTOCOL_VERSION`]; checked (with
        /// [`hello_protocol`]) before the rest of the message is parsed.
        protocol: u32,
        name: String,
        pairing_code: Option<String>,
        /// The viewer's screen (what Extend mode sizes a virtual monitor to).
        display: DisplayInfo,
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
        /// What arrived over the window, in kbit/s on the wire (first transmissions of video
        /// packets, parity included, headers included; resends do not count): the viewer's
        /// acknowledgement of what the presenter sent, a second signal for the adaptive bitrate.
        rx_kbps: u32,
        /// Length of the window the report covers, in ms (so the presenter can compare `rx_kbps`
        /// with what it sent in the same period).
        window_ms: u32,
        /// One-way queueing delay of the window's frames; `None` when no
        /// frame arrived in the window.
        queue: Option<QueueDelay>,
    },
    /// Viewer -> presenter, when the viewer's bottleneck verdict changes (and once on connect
    /// once it has one): the same coarse verdict the viewer shows on its HUD, so the presenter
    /// window can show it too. Both fields are required.
    Bottleneck {
        stage: BottleneckStage,
        rating: BottleneckRating,
    },
    Bye,
}

/// Which stage limits the stream, as judged by the viewer.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BottleneckStage {
    None,
    Network,
    Viewer,
    Driver,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BottleneckRating {
    Good,
    Ok,
    Weak,
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
            display: DisplayInfo {
                width: 1920,
                height: 1080,
                scale_milli: 1000,
            },
        };
        assert_eq!(
            hello_protocol(&hello.encode()[4..]),
            Some(crate::PROTOCOL_VERSION)
        );
    }

    #[test]
    fn display_info_is_bounded() {
        let d = |w, h, s| DisplayInfo {
            width: w,
            height: h,
            scale_milli: s,
        };
        assert!(d(1920, 1080, 1000).sanitized().is_some());
        assert!(d(0, 1080, 1000).sanitized().is_none());
        assert!(d(1920, 99_999, 1000).sanitized().is_none());
        assert!(d(1920, 1080, 0).sanitized().is_none());
        assert_eq!(
            DisplayInfo::from_points(1280.0, 720.0, 1.5),
            d(1920, 1080, 1500)
        );
        assert!(DisplayInfo::from_points(0.0, f32::NAN, 0.0)
            .sanitized()
            .is_none());
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
            rx_kbps: 21_500,
            window_ms: 503,
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
        assert!(
            text.contains(r#""rx_kbps":21500,"window_ms":503"#),
            "{text}"
        );
        assert_eq!(Control::decode_body(&body[4..]).unwrap(), report);

        let verdict = Control::Bottleneck {
            stage: BottleneckStage::Network,
            rating: BottleneckRating::Weak,
        };
        let body = verdict.encode();
        assert_eq!(
            std::str::from_utf8(&body[4..]).unwrap(),
            r#"{"type":"bottleneck","stage":"network","rating":"weak"}"#
        );
        assert_eq!(Control::decode_body(&body[4..]).unwrap(), verdict);
        // Unknown stages and missing fields are errors, not panics.
        assert!(
            Control::decode_body(br#"{"type":"bottleneck","stage":"gpu","rating":"weak"}"#)
                .is_err()
        );
        assert!(Control::decode_body(br#"{"type":"bottleneck","stage":"none"}"#).is_err());
        // Every field is required: a report of another protocol version does not parse.
        let old = br#"{"type":"link_report","loss_permille":5,"packets":9,"recovered":0,"unrecoverable":0,"queue":null}"#;
        assert!(Control::decode_body(old).is_err());
    }
}
