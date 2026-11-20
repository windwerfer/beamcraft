#![forbid(unsafe_code)]
//! Test support for the Miracast spike: a real-socket sink driver ([`run_sink`]) and a minimal
//! test source ([`TestSource`]) that speaks RTSP M1-M7 and sends MPEG-TS over RTP, all on
//! localhost, no Wi-Fi. This is test code: it panics on I/O errors instead of reporting them.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::time::{Duration, Instant};

use miracast_rtp::ts::synth::{packetize, pes, psi_packets, rtp_packets, PID_AUDIO, PID_VIDEO};
use miracast_rtp::{MediaSink, Receiver};
use miracast_rtsp::message::{parse_message, Message, Request, Response};
use miracast_rtsp::{Action, Negotiated, SinkConfig, SinkSession, State};

const FEATURE: &str = "org.wfa.wfd1.0";
const PUBLIC: &str = "org.wfa.wfd1.0, GET_PARAMETER, SET_PARAMETER";

/// What the sink side saw, returned by [`run_sink`].
#[derive(Debug, Default)]
pub struct SinkReport {
    pub negotiated: Option<Negotiated>,
    pub close_reason: Option<String>,
    pub idr_requests_sent: u32,
    pub lost_packets: u64,
    pub bad_datagrams: u64,
}

/// Drives a [`SinkSession`] and a [`Receiver`] over real sockets until the session closes
/// (or `deadline` passes). `udp` must be bound already; its port is announced as the RTP port.
pub fn run_sink(
    listener: TcpListener,
    udp: UdpSocket,
    sink: &mut dyn MediaSink,
    deadline: Duration,
) -> SinkReport {
    run_sink_logged(listener, udp, sink, deadline, &mut |_| {})
}

/// Like [`run_sink`], and calls `log` with the RTSP trace (both directions) and state events.
pub fn run_sink_logged(
    listener: TcpListener,
    udp: UdpSocket,
    sink: &mut dyn MediaSink,
    deadline: Duration,
    log: &mut dyn FnMut(String),
) -> SinkReport {
    let cfg = SinkConfig {
        rtp_port: udp.local_addr().unwrap().port(),
        ..SinkConfig::default()
    };
    let mut session = SinkSession::new(cfg);
    let mut rx = Receiver::new();
    let mut report = SinkReport::default();
    let start = Instant::now();

    let (mut tcp, peer) = listener.accept().unwrap();
    log(format!("RTSP connection from {peer}"));
    tcp.set_read_timeout(Some(Duration::from_millis(2)))
        .unwrap();
    udp.set_nonblocking(true).unwrap();
    let mut media = false;
    let mut buf = [0u8; 4096];
    let mut dgram = vec![0u8; 2048];
    let mut want_idr = false;
    let mut pending: Vec<Action> = Vec::new();

    'outer: while start.elapsed() < deadline {
        match tcp.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                log(format!(
                    "<<< source\n{}",
                    String::from_utf8_lossy(&buf[..n])
                ));
                pending.extend(session.handle_input(&buf[..n], Instant::now()));
            }
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(_) => break,
        }
        pending.extend(session.poll(Instant::now()));
        // Until PLAY is acknowledged leave datagrams in the socket buffer: a source may start
        // sending right after it receives the M7 reply, before the sink has processed it.
        while let Some(n) = media.then(|| udp.recv(&mut dgram).ok()).flatten() {
            let mut flagged = FlagSink {
                inner: sink,
                need_idr: &mut want_idr,
            };
            rx.push(&dgram[..n], &mut flagged);
        }
        if want_idr {
            want_idr = false;
            report.idr_requests_sent += 1;
            pending.extend(session.request_idr());
        }
        for a in pending.drain(..) {
            match a {
                Action::Send(b) => {
                    log(format!(">>> sink\n{}", String::from_utf8_lossy(&b)));
                    tcp.write_all(&b).unwrap();
                }
                Action::Negotiated(n) => {
                    log(format!("negotiated: {n:?}"));
                    report.negotiated = Some(n);
                }
                Action::StartMedia { rtp_port } => {
                    log(format!("start media on UDP {rtp_port}"));
                    media = true;
                }
                Action::StopMedia => {
                    log("stop media".into());
                    media = false;
                }
                Action::Close { reason } => {
                    log(format!("close: {reason}"));
                    report.close_reason = Some(reason);
                    break 'outer;
                }
            }
        }
        if session.state() == State::Closed {
            break;
        }
    }
    report.lost_packets = rx.lost_packets;
    report.bad_datagrams = rx.bad_datagrams;
    report
}

/// Forwards media and remembers `need_keyframe`.
struct FlagSink<'a> {
    inner: &'a mut dyn MediaSink,
    need_idr: &'a mut bool,
}

impl MediaSink for FlagSink<'_> {
    fn video(&mut self, unit: miracast_rtp::VideoAccessUnit) {
        self.inner.video(unit);
    }
    fn audio(&mut self, frame: miracast_rtp::AudioFrame) {
        self.inner.audio(frame);
    }
    fn need_keyframe(&mut self) {
        *self.need_idr = true;
        self.inner.need_keyframe();
    }
}

/// Annex B access unit: SPS, PPS, then an IDR (type 5) or non-IDR (type 1) slice.
pub fn access_unit(idr: bool, len: usize) -> Vec<u8> {
    let mut v = vec![0, 0, 0, 1, 0x67, 1, 2, 0, 0, 0, 1, 0x68, 3];
    v.extend_from_slice(&[0, 0, 0, 1, if idr { 0x65 } else { 0x41 }]);
    v.extend((0..len).map(|i| 0x10 + (i % 7) as u8));
    v
}

/// Minimal Miracast source: RTSP client side of M1-M7 plus an RTP/TS sender.
pub struct TestSource {
    tcp: TcpStream,
    buf: Vec<u8>,
    cseq: u32,
    udp: UdpSocket,
    rtp_target: Option<SocketAddr>,
    seq: u16,
    vcc: u8,
    acc: u8,
    pcc: u8,
    frame: usize,
    session: &'static str,
}

impl TestSource {
    pub fn connect(rtsp: SocketAddr) -> Self {
        let tcp = TcpStream::connect(rtsp).unwrap();
        tcp.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        Self {
            tcp,
            buf: Vec::new(),
            cseq: 0,
            udp: UdpSocket::bind("127.0.0.1:0").unwrap(),
            rtp_target: None,
            seq: 0,
            vcc: 0,
            acc: 0,
            pcc: 0,
            frame: 0,
            session: "6B8B4567",
        }
    }

    /// Blocks until one full RTSP message arrived.
    pub fn read_message(&mut self) -> Message {
        loop {
            if let Some((m, used)) = parse_message(&self.buf).unwrap() {
                self.buf.drain(..used);
                return m;
            }
            let mut b = [0u8; 2048];
            let n = self.tcp.read(&mut b).expect("RTSP read");
            assert!(n > 0, "sink closed the RTSP connection");
            self.buf.extend_from_slice(&b[..n]);
        }
    }

    fn next_cseq(&mut self) -> u32 {
        self.cseq += 1;
        self.cseq
    }

    fn send_request(&mut self, method: &str, uri: &str, body: &str) -> u32 {
        let cseq = self.next_cseq();
        let r = Request::new(method, uri, cseq)
            .with_header("Require", FEATURE)
            .with_params_body(body.to_string());
        self.tcp.write_all(&r.encode()).unwrap();
        cseq
    }

    fn expect_response(&mut self, cseq: u32) -> Response {
        match self.read_message() {
            Message::Response(r) => {
                assert_eq!(r.headers.cseq(), Some(cseq));
                r
            }
            m => panic!("expected response, got {m:?}"),
        }
    }

    fn expect_request(&mut self, method: &str) -> Request {
        match self.read_message() {
            Message::Request(r) => {
                assert_eq!(r.method, method);
                r
            }
            m => panic!("expected {method} request, got {m:?}"),
        }
    }

    /// Runs M1..M7 as the source; returns the sink's M3 answer body.
    pub fn negotiate(&mut self) -> String {
        let url = "rtsp://127.0.0.1/wfd1.0";
        // M1 source -> sink, then the sink's M2 to us.
        let c = self.send_request("OPTIONS", "*", "");
        let r = self.expect_response(c);
        assert_eq!(r.status, 200);
        assert_eq!(r.headers.get("Public"), Some(PUBLIC));
        let m2 = self.expect_request("OPTIONS");
        let ok = Response::new(200, m2.headers.cseq().unwrap()).with_header("Public", PUBLIC);
        self.tcp.write_all(&ok.encode()).unwrap();
        // M3 get capabilities.
        let c = self.send_request(
            "GET_PARAMETER",
            url,
            "wfd_client_rtp_ports\r\nwfd_audio_codecs\r\nwfd_video_formats\r\nwfd_content_protection\r\n",
        );
        let caps = self.expect_response(c);
        assert_eq!(caps.status, 200);
        // M4 choose 1080p30 CBP, no audio, our RTP port.
        let announced: u16 = caps
            .body
            .lines()
            .find_map(|l| l.strip_prefix("wfd_client_rtp_ports: RTP/AVP/UDP;unicast "))
            .and_then(|l| l.split(' ').next())
            .and_then(|p| p.parse().ok())
            .expect("sink announces an RTP port in M3");
        let body = format!("wfd_video_formats: 00 00 01 04 00000080 00000000 00000000 00 0000 0000 00 none none\r\nwfd_audio_codecs: none\r\nwfd_presentation_URL: rtsp://127.0.0.1/wfd1.0/streamid=0 none\r\nwfd_client_rtp_ports: RTP/AVP/UDP;unicast {announced} 0 mode=play\r\n");
        let c = self.send_request("SET_PARAMETER", url, &body);
        assert_eq!(self.expect_response(c).status, 200);
        // M5 trigger SETUP, then the sink's M6.
        let c = self.send_request("SET_PARAMETER", url, "wfd_trigger_method: SETUP\r\n");
        assert_eq!(self.expect_response(c).status, 200);
        let setup = self.expect_request("SETUP");
        let transport = setup
            .headers
            .get("Transport")
            .expect("Transport")
            .to_string();
        let port: u16 = transport
            .split("client_port=")
            .nth(1)
            .and_then(|s| s.split([';', '-']).next())
            .and_then(|s| s.parse().ok())
            .expect("client_port in Transport");
        self.rtp_target = Some(SocketAddr::from(([127, 0, 0, 1], port)));
        let ok = Response::new(200, setup.headers.cseq().unwrap())
            .with_header("Session", format!("{};timeout=30", self.session))
            .with_header(
                "Transport",
                format!("RTP/AVP/UDP;unicast;client_port={port};server_port=15550"),
            );
        self.tcp.write_all(&ok.encode()).unwrap();
        // M7.
        let play = self.expect_request("PLAY");
        assert_eq!(play.headers.get("Session"), Some(self.session));
        let ok = Response::new(200, play.headers.cseq().unwrap());
        self.tcp.write_all(&ok.encode()).unwrap();
        caps.body
    }

    /// Builds the RTP datagrams for the next video frame (and one audio PES).
    /// Every 5th frame is an IDR. Returns the access unit sent.
    pub fn next_frame(&mut self) -> (Vec<Vec<u8>>, Vec<u8>) {
        let i = self.frame;
        self.frame += 1;
        let au = access_unit(i.is_multiple_of(5), 200 + i * 90);
        let pts = 90_000 + i as u64 * 3000;
        let mut tsp = psi_packets(self.pcc);
        self.pcc = (self.pcc + 1) & 0x0F;
        tsp.extend(packetize(
            PID_VIDEO,
            &mut self.vcc,
            &pes(0xE0, pts, &au, false),
        ));
        tsp.extend(packetize(
            PID_AUDIO,
            &mut self.acc,
            &pes(0xC0, pts, &[0xFF, 0xF1, i as u8], true),
        ));
        (rtp_packets(&mut self.seq, pts as u32, &tsp), au)
    }

    pub fn send_datagrams(&self, d: &[Vec<u8>]) {
        let to = self.rtp_target.expect("negotiate first");
        for p in d {
            self.udp.send_to(p, to).unwrap();
            // Localhost UDP drops when the receive buffer fills: pace a little.
            std::thread::sleep(Duration::from_micros(200));
        }
    }

    /// Waits for the sink's M13 `wfd_idr_request`, answers it.
    pub fn expect_idr_request(&mut self) {
        let r = self.expect_request("SET_PARAMETER");
        assert!(r.body.contains("wfd_idr_request"), "body: {}", r.body);
        let ok = Response::new(200, r.headers.cseq().unwrap());
        self.tcp.write_all(&ok.encode()).unwrap();
    }

    /// M8: source-initiated teardown; returns after the sink's reply.
    pub fn teardown(&mut self) {
        let c = self.send_request("TEARDOWN", "rtsp://127.0.0.1/wfd1.0/streamid=0", "");
        let _ = self.expect_response(c);
    }
}
