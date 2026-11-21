//! Loopback end-to-end: test source -> TCP RTSP + UDP RTP/TS -> sink -> access units.
//! No Wi-Fi, no root, localhost only.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use std::net::{SocketAddr, TcpListener, UdpSocket};
use std::time::{Duration, Instant};

use miracast_loopback::{run_sink, SinkReport, TestSource};
use miracast_rtp::{AudioFrame, MediaSink, VideoAccessUnit};

#[derive(Default)]
struct Rec {
    video: Vec<VideoAccessUnit>,
    audio: Vec<AudioFrame>,
    idr_wanted: u32,
}

impl MediaSink for Rec {
    fn video(&mut self, u: VideoAccessUnit) {
        self.video.push(u);
    }
    fn audio(&mut self, f: AudioFrame) {
        self.audio.push(f);
    }
    fn need_keyframe(&mut self) {
        self.idr_wanted += 1;
    }
}

/// Starts the sink thread connecting to the source's RTSP server at `source`.
fn spawn_sink(
    source: SocketAddr,
    deadline: Duration,
) -> std::thread::JoinHandle<(SinkReport, Rec)> {
    let udp = UdpSocket::bind("127.0.0.1:0").unwrap();
    std::thread::spawn(move || {
        let mut rec = Rec::default();
        let report = run_sink(source, udp, &mut rec, deadline);
        (report, rec)
    })
}

/// A listening test source and a sink connected to it.
fn start() -> (TestSource, std::thread::JoinHandle<(SinkReport, Rec)>) {
    let listener = TestSource::listen();
    let sink = spawn_sink(listener.addr(), Duration::from_secs(20));
    (listener.accept(), sink)
}

#[test]
fn negotiates_and_streams_every_access_unit() {
    let (mut src, sink) = start();
    let caps = src.negotiate();
    assert!(caps.contains("wfd_video_formats"), "M3 answer: {caps}");
    assert!(
        caps.contains("wfd_audio_codecs: LPCM 00000002 00"),
        "M3 answer: {caps}"
    );

    let mut sent = Vec::new();
    for _ in 0..20 {
        let (d, au) = src.next_frame();
        src.send_datagrams(&d);
        sent.push(au);
    }
    std::thread::sleep(Duration::from_millis(200));
    src.teardown();

    let (report, rec) = sink.join().unwrap();
    let n = report.negotiated.as_ref().expect("negotiated");
    assert_eq!(n.resolution.unwrap().height, 1080);
    // The last unbounded video PES only flushes on the next one.
    assert_eq!(rec.video.len(), 19, "report: {report:?}");
    for (i, u) in rec.video.iter().enumerate() {
        assert_eq!(u.data, sent[i]);
        assert_eq!(u.keyframe, i.is_multiple_of(5));
    }
    assert_eq!(rec.audio.len(), 20);
    assert_eq!(report.lost_packets, 0);
    assert_eq!(report.bad_datagrams, 0);
    assert_eq!(report.idr_requests_sent, 0);
}

#[test]
fn loss_triggers_idr_request_and_recovery() {
    let (mut src, sink) = start();
    src.negotiate();

    let mut sent = Vec::new();
    for f in 0..16 {
        let (mut d, au) = src.next_frame();
        if f == 6 && d.len() > 1 {
            d.remove(1); // lose one RTP packet in frame 6
        }
        src.send_datagrams(&d);
        sent.push(au);
    }
    // Reorder window needs a few later packets to declare the gap lost; the stream above
    // supplies them. The sink must now ask for an IDR over RTSP (M13).
    src.expect_idr_request();
    std::thread::sleep(Duration::from_millis(100));
    src.teardown();

    let (report, rec) = sink.join().unwrap();
    assert!(report.lost_packets >= 1, "{report:?}");
    assert!(rec.idr_wanted >= 1);
    assert_eq!(report.idr_requests_sent, rec.idr_wanted);
    // Frames before the loss arrive intact; decoding resumes at the next IDR (frame 10, 15).
    assert_eq!(rec.video[0].data, sent[0]);
    assert!(rec.video.iter().any(|u| u.keyframe && u.data == sent[10]));
    assert!(rec.video.iter().all(|u| u.data != sent[7] || u.keyframe));
}

#[test]
fn bad_datagrams_are_counted_not_fatal() {
    let (mut src, sink) = start();
    src.negotiate();
    src.send_datagrams(&[vec![1, 2, 3], vec![0x80, 33, 0, 0], vec![0xFF; 300]]);
    let (d, _) = src.next_frame();
    src.send_datagrams(&d);
    std::thread::sleep(Duration::from_millis(100));
    src.teardown();
    let (report, _) = sink.join().unwrap();
    assert_eq!(report.bad_datagrams, 3);
}

#[test]
fn sink_retries_until_the_source_listens() {
    // Reserve a free port, release it, and only listen on it after the sink started trying.
    let addr = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap();
    let sink = spawn_sink(addr, Duration::from_secs(20));
    std::thread::sleep(Duration::from_millis(700));
    let mut src = TestSource::listen_on(addr).accept();
    src.negotiate();
    src.teardown();
    let (report, _) = sink.join().unwrap();
    assert!(report.negotiated.is_some(), "{report:?}");
}

#[test]
fn unreachable_source_is_a_readable_close_reason() {
    let addr = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap();
    let t = Instant::now();
    let (report, _) = spawn_sink(addr, Duration::from_millis(1200))
        .join()
        .unwrap();
    assert!(t.elapsed() < Duration::from_secs(5));
    let reason = report.close_reason.expect("close reason");
    assert!(
        reason.contains("cannot reach the source's RTSP server"),
        "{reason}"
    );
    assert!(reason.contains(&addr.to_string()), "{reason}");
}
