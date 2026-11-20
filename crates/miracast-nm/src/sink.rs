use crate::{wfd, GroupInfo, P2pError, P2pEvent, P2pProvider};

/// What the sink announces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SinkConfig {
    /// Name shown in the source's cast list.
    pub name: String,
    /// RTSP listen port (TCP), advertised in the WFD IE.
    pub rtsp_port: u16,
    /// UDP port the source sends RTP to (announced in `wfd_client_rtp_ports`).
    pub rtp_port: u16,
    pub max_throughput_mbps: u16,
}

impl Default for SinkConfig {
    fn default() -> Self {
        Self {
            name: "MirrorWire".into(),
            rtsp_port: wfd::DEFAULT_RTSP_PORT,
            rtp_port: 19000,
            max_throughput_mbps: 50,
        }
    }
}

impl SinkConfig {
    /// WFD subelements to publish.
    pub fn wfd_ies(&self) -> Vec<u8> {
        wfd::sink_device_info(self.rtsp_port, self.max_throughput_mbps)
    }
}

/// Sink-side P2P lifecycle: advertise, wait for a group, tear down. The RTSP/RTP crates take
/// over from the returned [`GroupInfo`].
pub struct Sink<P: P2pProvider> {
    provider: P,
    config: SinkConfig,
}

impl<P: P2pProvider> Sink<P> {
    pub fn new(provider: P, config: SinkConfig) -> Self {
        Self { provider, config }
    }

    pub fn config(&self) -> &SinkConfig {
        &self.config
    }

    /// Publish the WFD IE and start discovery so sources list us.
    pub async fn start(&self) -> Result<(), P2pError> {
        self.provider.set_wfd_ies(&self.config.wfd_ies()).await?;
        self.provider.start_find().await
    }

    /// Wait until a P2P group exists (formed by the source or by us). `Ok(None)` if the
    /// provider closed first.
    pub async fn wait_for_group(&self) -> Result<Option<GroupInfo>, P2pError> {
        while let Some(ev) = self.provider.next_event().await {
            if let P2pEvent::GroupFormed(g) = ev {
                self.provider.stop_find().await?;
                return Ok(Some(g));
            }
        }
        Ok(None)
    }

    /// Leave the group and stop discovering.
    pub async fn stop(&self) -> Result<(), P2pError> {
        self.provider.stop_find().await?;
        self.provider.disconnect().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mock::MockCall;
    use crate::{GroupRole, MockP2pProvider, PeerInfo};
    use std::net::Ipv4Addr;

    fn group() -> GroupInfo {
        GroupInfo {
            interface: "p2p-wlp3s0-0".into(),
            role: GroupRole::Client,
            local_ip: Ipv4Addr::new(192, 168, 49, 2),
            peer_ip: Some(Ipv4Addr::new(192, 168, 49, 1)),
        }
    }

    #[tokio::test]
    async fn start_publishes_ie_then_finds() {
        let sink = Sink::new(MockP2pProvider::new(), SinkConfig::default());
        sink.start().await.unwrap();
        let calls = sink.provider.calls();
        assert_eq!(calls[0], MockCall::SetWfdIes(sink.config().wfd_ies()));
        assert_eq!(calls[1], MockCall::StartFind);
    }

    #[tokio::test]
    async fn incoming_group_is_returned_and_find_stops() {
        let mock = MockP2pProvider::new();
        mock.push_event(P2pEvent::PeerAdded(PeerInfo {
            id: "/p/1".into(),
            name: "TB336FU".into(),
            hw_address: "02:00:00:00:00:01".into(),
            wfd_ies: vec![],
            strength: 70,
        }));
        mock.push_event(P2pEvent::GroupFormed(group()));
        let sink = Sink::new(mock, SinkConfig::default());
        assert_eq!(sink.wait_for_group().await.unwrap(), Some(group()));
        assert!(sink.provider.calls().contains(&MockCall::StopFind));
    }

    #[tokio::test]
    async fn closed_provider_yields_none() {
        let sink = Sink::new(MockP2pProvider::new(), SinkConfig::default());
        assert_eq!(sink.wait_for_group().await.unwrap(), None);
    }
}
