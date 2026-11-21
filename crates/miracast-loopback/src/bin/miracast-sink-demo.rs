//! Hardware test sink: connects to one Miracast source's RTSP server (retrying until the
//! source listens), prints the RTSP trace and media stats, and writes the received H.264
//! (Annex B) to a file playable with `ffplay`/`mpv`. Without `--nm`, bring the Wi-Fi
//! Direct link up first (see HANDOFF.md). The source is the group owner, on Android
//! `192.168.49.1`.
//!
//! With `--nm` it does the Wi-Fi Direct part itself through NetworkManager (no root): find a
//! source, join its group with our WFD IEs (the user taps this device in the source's Cast
//! list), connect to the group owner's RTSP port, and leave the group at the end.
//!
//! Usage: `miracast-sink-demo [--nm [--peer NAME] [--find-secs 60] [--attempts 3]]
//!         [--source 192.168.49.1:7236] [--rtp-port 19000] [--out stream.h264] [--secs 600]`
#![forbid(unsafe_code)]
#![allow(clippy::print_stderr)]

use std::fs::File;
use std::io::Write;
use std::net::{SocketAddr, UdpSocket};
use std::time::{Duration, Instant};

use miracast_loopback::{run_sink_logged, RunOptions};
use miracast_rtp::h264::{find_sps, Sps};
use miracast_rtp::{AudioFrame, MediaSink, VideoAccessUnit};

struct Out {
    file: File,
    start: Instant,
    video: u64,
    audio: u64,
    bytes: u64,
    idr: u64,
    last_report: Instant,
    /// Size the decoder from this, not from M4 (run 3: M4 level 3.2, SPS level 4.0).
    sps: Option<Sps>,
}

impl Out {
    fn maybe_report(&mut self) {
        if self.last_report.elapsed() >= Duration::from_secs(1) {
            self.last_report = Instant::now();
            eprintln!(
                "[{:6.1}s] video AUs {} ({} B), audio frames {}, loss-driven IDR requests {}",
                self.start.elapsed().as_secs_f32(),
                self.video,
                self.bytes,
                self.audio,
                self.idr
            );
        }
    }
}

impl MediaSink for Out {
    fn video(&mut self, u: VideoAccessUnit) {
        self.video += 1;
        self.bytes += u.data.len() as u64;
        if let Some(sps) = u.keyframe.then(|| find_sps(&u.data)).flatten() {
            if self.sps != Some(sps) {
                let (major, minor) = sps.level();
                eprintln!(
                    "[{:6.1}s] stream SPS: {}x{} profile {} level {major}.{minor}",
                    self.start.elapsed().as_secs_f32(),
                    sps.width,
                    sps.height,
                    sps.profile_idc
                );
                self.sps = Some(sps);
            }
        }
        if let Err(e) = self.file.write_all(&u.data) {
            eprintln!("cannot write output: {e}");
        }
        self.maybe_report();
    }
    fn audio(&mut self, _: AudioFrame) {
        self.audio += 1;
    }
    fn need_keyframe(&mut self) {
        self.idr += 1;
    }
}

fn arg(name: &str, default: &str) -> String {
    let a: Vec<String> = std::env::args().collect();
    a.iter()
        .position(|x| x == name)
        .and_then(|i| a.get(i + 1))
        .cloned()
        .unwrap_or_else(|| default.to_string())
}

fn has_flag(name: &str) -> bool {
    std::env::args().any(|a| a == name)
}

fn log_line(start: Instant, l: &str) {
    eprintln!("[{:6.1}s] {l}", start.elapsed().as_secs_f32());
}

/// `--nm`: find the source over Wi-Fi Direct and join its group (R1/R2 flow). Returns the
/// sink (to leave the group afterwards), the source's RTSP address and the group interface.
#[cfg(target_os = "linux")]
fn join_with_nm(
    rt: &tokio::runtime::Runtime,
    start: Instant,
) -> Result<(NmSink, SocketAddr, String), String> {
    use miracast_loopback::nm_flow::{join_source, FlowOptions};
    use miracast_nm::zbus_backend::ZbusNm;
    use miracast_nm::{NmP2pProvider, Sink, SinkConfig};

    let opts = FlowOptions {
        peer_name: std::env::args()
            .any(|a| a == "--peer")
            .then(|| arg("--peer", "")),
        find_timeout: Duration::from_secs(arg("--find-secs", "60").parse().unwrap_or(60)),
        attempts: arg("--attempts", "3").parse().unwrap_or(3),
        ..FlowOptions::default()
    };
    rt.block_on(async {
        let api = ZbusNm::connect_system().await.map_err(|e| e.to_string())?;
        let sink = Sink::new(NmP2pProvider::new(api), SinkConfig::default());
        let mut log = |l: String| log_line(start, &l);
        match join_source(&sink, &opts, &mut log).await {
            Ok(j) => Ok((sink, j.rtsp, j.group.interface)),
            Err(e) => Err(e.to_string()),
        }
    })
}

#[cfg(target_os = "linux")]
type NmSink = miracast_nm::Sink<miracast_nm::NmP2pProvider<miracast_nm::zbus_backend::ZbusNm>>;

fn main() {
    let nm = has_flag("--nm");
    let source = arg("--source", "192.168.49.1:7236");
    let rtp = arg("--rtp-port", "19000");
    let out = arg("--out", "stream.h264");
    let secs: u64 = arg("--secs", "600").parse().unwrap_or(600);
    let source: SocketAddr = match source.parse() {
        Ok(a) => a,
        Err(e) => return eprintln!("--source {source}: not an IP:port ({e})"),
    };
    let udp = match UdpSocket::bind(format!("0.0.0.0:{rtp}")) {
        Ok(u) => u,
        Err(e) => return eprintln!("cannot bind UDP {rtp}: {e}"),
    };
    let file = match File::create(&out) {
        Ok(f) => f,
        Err(e) => return eprintln!("cannot create {out}: {e}"),
    };
    let now = Instant::now();
    let mut opts = RunOptions::new(Duration::from_secs(secs));
    let mut source = source;

    #[cfg(target_os = "linux")]
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => return eprintln!("cannot start the async runtime: {e}"),
    };
    #[cfg(target_os = "linux")]
    let mut joined: Option<NmSink> = None;
    if nm {
        #[cfg(target_os = "linux")]
        match join_with_nm(&rt, now) {
            Ok((sink, rtsp, iface)) => {
                source = rtsp;
                opts.interface = Some(iface);
                joined = Some(sink);
            }
            Err(e) => return log_line(now, &format!("--nm failed: {e}")),
        }
        #[cfg(not(target_os = "linux"))]
        return eprintln!("--nm needs NetworkManager (Linux)");
    }

    let mut sink = Out {
        file,
        start: now,
        video: 0,
        audio: 0,
        bytes: 0,
        idr: 0,
        last_report: now,
        sps: None,
    };
    eprintln!("connecting to the source at {source}, RTP on UDP {rtp}, writing {out}");
    let report = run_sink_logged(source, udp, &mut sink, &opts, &mut |l| log_line(now, &l));
    eprintln!("done: {report:?}");

    #[cfg(target_os = "linux")]
    if let Some(s) = joined {
        match rt.block_on(s.stop()) {
            Ok(()) => log_line(now, "Wi-Fi Direct group left"),
            Err(e) => log_line(now, &format!("leaving the Wi-Fi Direct group failed: {e}")),
        }
    }
}
