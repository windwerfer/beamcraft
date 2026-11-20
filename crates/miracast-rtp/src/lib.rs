#![forbid(unsafe_code)]
//! Miracast media receive: RTP (RFC 3550) carrying MPEG-TS (ISO 13818-1) with H.264 video and
//! audio PES, delivered as access units (step 5; `docs/agents/miracast.md` section 4).
//!
//! Skeleton (step 3): only the output types and the sink trait the receiver will call. The
//! receiver gets bytes from a UDP socket owned by the caller (sans-IO), so it tests from
//! synthetic streams.

/// Network/port settings the RTSP layer negotiated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RtpConfig {
    /// UDP port the source sends to (`wfd_client_rtp_ports`, default 19000).
    pub rtp_port: u16,
}

impl Default for RtpConfig {
    fn default() -> Self {
        Self { rtp_port: 19000 }
    }
}

/// One H.264 access unit in Annex B form (start codes), ready for a decoder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoAccessUnit {
    /// Presentation timestamp, 90 kHz ticks.
    pub pts_90khz: u64,
    pub keyframe: bool,
    pub data: Vec<u8>,
}

/// One audio PES payload (LPCM or AAC ADTS); demuxed but not played (non-goal).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioFrame {
    pub pts_90khz: u64,
    pub data: Vec<u8>,
}

/// Consumer of demuxed media. MirrorWire's decoder path implements this later.
pub trait MediaSink {
    fn video(&mut self, unit: VideoAccessUnit);
    fn audio(&mut self, frame: AudioFrame);
    /// Loss or a continuity error made the next video unit undecodable: the RTSP layer should
    /// send `wfd_idr_request` (M13).
    fn need_keyframe(&mut self);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_port_is_the_wfd_default() {
        assert_eq!(RtpConfig::default().rtp_port, 19000);
    }
}
