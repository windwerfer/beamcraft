//! `--nm` flow of `miracast-sink-demo`: find a Miracast source over Wi-Fi Direct, join its
//! group with our WFD IEs (R1/R2 flow, miracast.md section 11) and return the address of its
//! RTSP server. Generic over [`P2pProvider`], so it tests with [`miracast_nm::MockP2pProvider`].

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use miracast_nm::{GroupInfo, P2pError, P2pProvider, PeerInfo, Sink};

/// How to pick and wait for the source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlowOptions {
    /// Only sources whose name contains this (case-insensitive); else the strongest one.
    pub peer_name: Option<String>,
    /// How long to look for a source.
    pub find_timeout: Duration,
    /// Pause between peer list reads.
    pub poll: Duration,
}

impl Default for FlowOptions {
    fn default() -> Self {
        Self {
            peer_name: None,
            find_timeout: Duration::from_secs(60),
            poll: Duration::from_secs(1),
        }
    }
}

/// A joined group and where the source's RTSP server is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Joined {
    pub peer: PeerInfo,
    pub group: GroupInfo,
    /// `peer_ip:port`, the port from the source's WFD IE.
    pub rtsp: SocketAddr,
}

/// The strongest source matching `name` (if given).
pub fn pick_source(sources: &[(PeerInfo, u16)], name: Option<&str>) -> Option<(PeerInfo, u16)> {
    let want = name.map(str::to_lowercase);
    sources
        .iter()
        .filter(|(p, _)| {
            want.as_deref()
                .is_none_or(|w| p.name.to_lowercase().contains(w))
        })
        .max_by_key(|(p, _)| p.strength)
        .cloned()
}

/// Publishes our IEs, finds a source, joins its group. On failure after discovery started,
/// discovery and any half-made group are stopped again.
pub async fn join_source<P: P2pProvider>(
    sink: &Sink<P>,
    opts: &FlowOptions,
    log: &mut (dyn FnMut(String) + Send),
) -> Result<Joined, P2pError> {
    sink.start().await?;
    log("Wi-Fi Direct discovery started; enable wireless display on the source".into());
    let start = Instant::now();
    let (peer, port) = loop {
        let sources = sink.sources().await?;
        if let Some(found) = pick_source(&sources, opts.peer_name.as_deref()) {
            break found;
        }
        if start.elapsed() >= opts.find_timeout {
            let _ = sink.stop().await;
            return Err(P2pError::Timeout(format!(
                "no Miracast source found in {} s{}: enable wireless display (Cast) on the source",
                opts.find_timeout.as_secs(),
                opts.peer_name
                    .as_deref()
                    .map(|n| format!(" with a name containing \"{n}\""))
                    .unwrap_or_default()
            )));
        }
        tokio::time::sleep(opts.poll).await;
    };
    log(format!(
        "source {} ({}), RTSP port {port}: connecting; on the source open Cast and tap this \
         device (do not answer its connect prompt)",
        peer.name, peer.hw_address
    ));
    let group = match sink.connect_source(&peer.id).await {
        Ok(g) => g,
        Err(e) => {
            let _ = sink.stop().await;
            return Err(e);
        }
    };
    log(format!("group: {group:?}"));
    let Some(ip) = group.peer_ip else {
        let _ = sink.stop().await;
        return Err(P2pError::Unsupported(format!(
            "we became Wi-Fi Direct group owner on {}, so the source's address is unknown \
             (only the source-as-owner case is implemented)",
            group.interface
        )));
    };
    Ok(Joined {
        rtsp: SocketAddr::from((ip, port)),
        peer,
        group,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use miracast_nm::{GroupRole, MockCall, MockP2pProvider, SinkConfig};
    use std::net::Ipv4Addr;

    fn tablet(id: &str, name: &str, strength: u8) -> PeerInfo {
        PeerInfo {
            id: id.into(),
            name: name.into(),
            hw_address: "7A:1B:85:73:E1:76".into(),
            wfd_ies: vec![0, 0, 6, 0, 0x10, 0x1c, 0x44, 0, 0x32],
            strength,
        }
    }

    fn client_group() -> GroupInfo {
        GroupInfo {
            interface: "p2p-wlp58s0-3".into(),
            role: GroupRole::Client,
            local_ip: Ipv4Addr::new(192, 168, 49, 74),
            peer_ip: Some(Ipv4Addr::new(192, 168, 49, 1)),
        }
    }

    fn fast() -> FlowOptions {
        FlowOptions {
            find_timeout: Duration::from_millis(30),
            poll: Duration::from_millis(5),
            ..FlowOptions::default()
        }
    }

    #[test]
    fn picks_by_name_then_strength() {
        let s = vec![
            (tablet("/a", "Android_qqQ0", 50), 7236),
            (tablet("/b", "Pixel", 90), 7236),
        ];
        assert_eq!(pick_source(&s, None).unwrap().0.id, "/b");
        assert_eq!(pick_source(&s, Some("android")).unwrap().0.id, "/a");
        assert!(pick_source(&s, Some("nope")).is_none());
        assert!(pick_source(&[], None).is_none());
    }

    #[tokio::test]
    async fn joins_the_source_and_targets_the_group_owner() {
        let mock = MockP2pProvider::new();
        mock.add_peer(tablet("/p/3", "Android_qqQ0", 97));
        mock.set_group(client_group());
        let sink = Sink::new(mock, SinkConfig::default());
        let mut lines = Vec::new();
        let j = join_source(&sink, &fast(), &mut |l| lines.push(l))
            .await
            .unwrap();
        assert_eq!(j.rtsp, "192.168.49.1:7236".parse().unwrap());
        assert_eq!(j.group.interface, "p2p-wlp58s0-3");
        assert!(
            lines.iter().any(|l| l.contains("tap this device")),
            "{lines:?}"
        );
    }

    #[tokio::test]
    async fn no_source_times_out_readably_and_stops_find() {
        let mock = MockP2pProvider::new();
        // A sink is not a source.
        mock.add_peer(PeerInfo {
            wfd_ies: miracast_nm::wfd::sink_device_info(7236, 50),
            ..tablet("/s", "OtherSink", 80)
        });
        let sink = Sink::new(mock, SinkConfig::default());
        let err = join_source(&sink, &fast(), &mut |_| {}).await.unwrap_err();
        assert!(matches!(err, P2pError::Timeout(_)), "{err}");
        assert!(err.to_string().contains("enable wireless display"), "{err}");
        assert!(sink.provider().calls().contains(&MockCall::StopFind));
    }

    #[tokio::test]
    async fn group_owner_role_is_refused_and_cleaned_up() {
        let mock = MockP2pProvider::new();
        mock.add_peer(tablet("/p/3", "Android_qqQ0", 97));
        mock.set_group(GroupInfo {
            role: GroupRole::Owner,
            peer_ip: None,
            ..client_group()
        });
        let sink = Sink::new(mock, SinkConfig::default());
        let err = join_source(&sink, &fast(), &mut |_| {}).await.unwrap_err();
        assert!(matches!(err, P2pError::Unsupported(_)), "{err}");
        assert_eq!(
            sink.provider().calls().last(),
            Some(&MockCall::Disconnect),
            "the group is left again"
        );
    }
}
