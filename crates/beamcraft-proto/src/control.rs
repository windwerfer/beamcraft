use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

pub const MAX_CONTROL_MESSAGE: usize = 64 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Control {
    Hello {
        protocol: u32,
        name: String,
        pairing_code: Option<String>,
        /// Optional features this viewer supports, e.g.
        /// [`crate::cursor::CAP_CURSOR_METADATA`]. Older viewers send none.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        caps: Vec<String>,
    },
    Rejected { reason: String },
    StreamInfo(StreamInfo),
    RequestKeyframe { reason: String },
    Ping { t_viewer_us: u64 },
    Pong { t_viewer_us: u64, t_presenter_us: u64 },
    /// Viewer -> presenter, about twice a second, only when the presenter's
    /// `stream_info` has a `fec` field (older presenters would reject an
    /// unknown message). Feeds the adaptive FEC overhead.
    LinkReport {
        /// Packet loss over the window, in 1/1000.
        loss_permille: u32,
        /// Packets the presenter sent for frames in the window.
        packets: u32,
        /// Data packets rebuilt from parity.
        recovered: u32,
        /// Frames that could not be rebuilt.
        unrecoverable: u32,
        /// Mean one-way queueing delay over the window (µs above the lowest
        /// delay of the last 30 s; [`crate::delay`]). Absent from older viewers.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        queue_us: Option<u32>,
        /// Highest queueing delay of a frame in the window (µs).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        queue_max_us: Option<u32>,
        /// Slope of the queueing delay over the window (µs per second;
        /// positive = a queue is building up).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        queue_trend_us_per_s: Option<i32>,
    },
    Bye,
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
    /// How the cursor reaches this viewer: [`CURSOR_SEPARATE`] (pointer
    /// datagrams + image streams), [`CURSOR_EMBEDDED`] (in the video) or
    /// [`CURSOR_HIDDEN`]. Absent from older presenters (= embedded).
    /// The presenter may send `stream_info` again when this changes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    /// Forward error correction setting (`off`, `auto`, `20%`) of a presenter
    /// that can send FEC packets and reads `link_report`. Absent from older
    /// presenters. Whether this viewer gets FEC packets depends on its hello
    /// listing [`crate::fec::CAP_FEC`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fec: Option<String>,
}

pub const CURSOR_SEPARATE: &str = "separate";
pub const CURSOR_EMBEDDED: &str = "embedded";
pub const CURSOR_HIDDEN: &str = "hidden";

impl Control {
    pub fn encode(&self) -> Vec<u8> {
        let body = serde_json::to_vec(self).expect("control message serializes");
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
    fn hello_caps_are_backward_compatible() {
        // An old viewer's hello (no caps) still parses, with empty caps.
        let old = br#"{"type":"hello","protocol":1,"name":"imac","pairing_code":null}"#;
        let Control::Hello { caps, .. } = Control::decode_body(old).unwrap() else { panic!() };
        assert!(caps.is_empty());
        // A new hello with caps parses for a presenter that ignores unknown fields.
        let new = Control::Hello {
            protocol: 1,
            name: "imac".into(),
            pairing_code: None,
            caps: vec![crate::cursor::CAP_CURSOR_METADATA.into()],
        };
        let body = &new.encode()[4..];
        assert!(std::str::from_utf8(body).unwrap().contains("cursor-metadata"));
        assert_eq!(Control::decode_body(body).unwrap(), new);
        // Old presenter stream_info (no cursor field) parses as None.
        let info = br#"{"type":"stream_info","width":1,"height":1,"fps":60,"codec":"h264","encoder":"x","bitrate_kbps":1,"intra_refresh":false}"#;
        let Control::StreamInfo(info) = Control::decode_body(info).unwrap() else { panic!() };
        assert_eq!(info.cursor, None);
        assert_eq!(info.fec, None);
        let report = Control::LinkReport {
            loss_permille: 20,
            packets: 1000,
            recovered: 18,
            unrecoverable: 0,
            queue_us: None,
            queue_max_us: None,
            queue_trend_us_per_s: None,
        };
        let body = report.encode();
        assert!(!std::str::from_utf8(&body[4..]).unwrap().contains("queue"), "old shape on the wire");
        assert_eq!(Control::decode_body(&body[4..]).unwrap(), report);
    }

    #[test]
    fn link_report_queue_delay_is_optional() {
        // An older viewer's report (no delay fields) parses with None.
        let old = br#"{"type":"link_report","loss_permille":5,"packets":900,"recovered":2,"unrecoverable":0}"#;
        let Control::LinkReport { queue_us, queue_max_us, queue_trend_us_per_s, .. } = Control::decode_body(old).unwrap()
        else {
            panic!()
        };
        assert_eq!((queue_us, queue_max_us, queue_trend_us_per_s), (None, None, None));
        // The new fields round-trip, and their names are pinned.
        let new = Control::LinkReport {
            loss_permille: 5,
            packets: 900,
            recovered: 2,
            unrecoverable: 0,
            queue_us: Some(12_000),
            queue_max_us: Some(30_000),
            queue_trend_us_per_s: Some(-4_500),
        };
        let body = new.encode();
        let text = std::str::from_utf8(&body[4..]).unwrap();
        assert!(text.contains(r#""queue_us":12000"#) && text.contains(r#""queue_max_us":30000"#), "{text}");
        assert!(text.contains(r#""queue_trend_us_per_s":-4500"#), "{text}");
        assert_eq!(Control::decode_body(&body[4..]).unwrap(), new);
    }
}
