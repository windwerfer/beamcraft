//! Hardware test sink: connects to one Miracast source's RTSP server (retrying until the
//! source listens), prints the RTSP trace and media stats, and writes the received H.264
//! (Annex B) to a file playable with `ffplay`/`mpv`. No P2P handling: bring the Wi-Fi
//! Direct link up first (see HANDOFF.md). The source is the group owner, on Android
//! `192.168.49.1`.
//!
//! Usage: `miracast-sink-demo [--source 192.168.49.1:7236] [--rtp-port 19000]
//!         [--out stream.h264] [--secs 600]`
#![forbid(unsafe_code)]
#![allow(clippy::print_stderr)]

use std::fs::File;
use std::io::Write;
use std::net::{SocketAddr, UdpSocket};
use std::time::{Duration, Instant};

use miracast_loopback::run_sink_logged;
use miracast_rtp::{AudioFrame, MediaSink, VideoAccessUnit};

struct Out {
    file: File,
    start: Instant,
    video: u64,
    audio: u64,
    bytes: u64,
    idr: u64,
    last_report: Instant,
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

fn main() {
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
    let mut sink = Out {
        file,
        start: now,
        video: 0,
        audio: 0,
        bytes: 0,
        idr: 0,
        last_report: now,
    };
    eprintln!("connecting to the source at {source}, RTP on UDP {rtp}, writing {out}");
    let report = run_sink_logged(
        source,
        udp,
        &mut sink,
        Duration::from_secs(secs),
        &mut |l| {
            eprintln!("[{:6.1}s] {l}", now.elapsed().as_secs_f32());
        },
    );
    eprintln!("done: {report:?}");
}
