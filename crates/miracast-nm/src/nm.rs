//! NetworkManager mapping of [`P2pProvider`]. NM's object model (devices, peers, connection
//! profiles) is behind [`NmApi`] so the logic here is tested against a recording mock; the
//! real D-Bus calls live in `zbus_backend`.

use crate::{BoxFuture, GroupInfo, P2pError, P2pEvent, P2pProvider, PeerInfo, WpsMethod};
use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::Duration;

/// A peer as NM reports it (`org.freedesktop.NetworkManager.WifiP2PPeer`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NmPeer {
    /// D-Bus object path.
    pub path: String,
    pub name: String,
    pub hw_address: String,
    pub wfd_ies: Vec<u8>,
    pub strength: u8,
}

/// The volatile `wifi-p2p` connection profile we ask NM to activate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct P2pConnectionSpec {
    /// `connection.id`.
    pub id: String,
    /// `wifi-p2p.peer`: the peer's P2P device address.
    pub peer: String,
    pub wps: WpsMethod,
    /// `wifi-p2p.wfd-ies`.
    pub wfd_ies: Vec<u8>,
}

impl P2pConnectionSpec {
    /// NM `NMSettingWifiP2PWpsMethod` value for `wifi-p2p.wps-method`.
    pub fn wps_method_value(&self) -> u32 {
        match self.wps {
            WpsMethod::Auto => 0x0, // default: NM decides
            WpsMethod::Pbc => 0x4,
        }
    }
}

/// The NetworkManager operations the provider needs. Implemented by the zbus backend (Linux)
/// and by test mocks. `device` is the D-Bus path of a `WifiP2P` device.
pub trait NmApi: Send + Sync {
    /// Paths of devices of type WIFI_P2P; empty if the hardware or supplicant lacks P2P.
    fn p2p_devices(&self) -> BoxFuture<'_, Result<Vec<String>, P2pError>>;
    /// `Device.WifiP2P.StartFind` with a timeout.
    fn start_find<'a>(
        &'a self,
        device: &'a str,
        timeout: Duration,
    ) -> BoxFuture<'a, Result<(), P2pError>>;
    fn stop_find<'a>(&'a self, device: &'a str) -> BoxFuture<'a, Result<(), P2pError>>;
    fn peers<'a>(&'a self, device: &'a str) -> BoxFuture<'a, Result<Vec<NmPeer>, P2pError>>;
    /// Advertise WFD IEs for the sink (see miracast.md risk R2 for how NM takes them).
    fn set_wfd_ies<'a>(
        &'a self,
        device: &'a str,
        ies: &'a [u8],
    ) -> BoxFuture<'a, Result<(), P2pError>>;
    /// `AddAndActivateConnection` for a volatile profile, then wait for the group and its
    /// addresses.
    fn connect<'a>(
        &'a self,
        device: &'a str,
        spec: &'a P2pConnectionSpec,
    ) -> BoxFuture<'a, Result<GroupInfo, P2pError>>;
    /// Deactivate whatever is active on the device.
    fn disconnect<'a>(&'a self, device: &'a str) -> BoxFuture<'a, Result<(), P2pError>>;
    /// Next change signal translated to an event; `None` when closed.
    fn next_event(&self) -> BoxFuture<'_, Option<P2pEvent>>;
}

/// How long one find runs before the backend must restart it.
const FIND_TIMEOUT: Duration = Duration::from_secs(120);

/// [`P2pProvider`] over NetworkManager. Uses the first P2P device.
pub struct NmP2pProvider<A: NmApi> {
    api: A,
    /// WFD IEs last set; reused in outgoing connection profiles.
    wfd_ies: Mutex<Vec<u8>>,
}

impl<A: NmApi> NmP2pProvider<A> {
    pub fn new(api: A) -> Self {
        Self {
            api,
            wfd_ies: Mutex::new(Vec::new()),
        }
    }

    async fn device(&self) -> Result<String, P2pError> {
        self.api
            .p2p_devices()
            .await?
            .into_iter()
            .next()
            .ok_or_else(|| {
                P2pError::NotAvailable(
                    "NetworkManager has no Wi-Fi P2P device (check wpa_supplicant P2P support)"
                        .into(),
                )
            })
    }

    fn ies(&self) -> Vec<u8> {
        self.wfd_ies
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
}

impl From<NmPeer> for PeerInfo {
    fn from(p: NmPeer) -> Self {
        PeerInfo {
            id: p.path,
            name: p.name,
            hw_address: p.hw_address,
            wfd_ies: p.wfd_ies,
            strength: p.strength,
        }
    }
}

impl<A: NmApi> P2pProvider for NmP2pProvider<A> {
    fn set_wfd_ies<'a>(&'a self, ies: &'a [u8]) -> BoxFuture<'a, Result<(), P2pError>> {
        Box::pin(async move {
            let dev = self.device().await?;
            self.api.set_wfd_ies(&dev, ies).await?;
            *self.wfd_ies.lock().unwrap_or_else(|e| e.into_inner()) = ies.to_vec();
            Ok(())
        })
    }

    fn start_find(&self) -> BoxFuture<'_, Result<(), P2pError>> {
        Box::pin(async move {
            let dev = self.device().await?;
            self.api.start_find(&dev, FIND_TIMEOUT).await
        })
    }

    fn stop_find(&self) -> BoxFuture<'_, Result<(), P2pError>> {
        Box::pin(async move {
            let dev = self.device().await?;
            self.api.stop_find(&dev).await
        })
    }

    fn peers(&self) -> BoxFuture<'_, Result<Vec<PeerInfo>, P2pError>> {
        Box::pin(async move {
            let dev = self.device().await?;
            Ok(self
                .api
                .peers(&dev)
                .await?
                .into_iter()
                .map(Into::into)
                .collect())
        })
    }

    fn connect<'a>(
        &'a self,
        peer_id: &'a str,
        wps: WpsMethod,
    ) -> BoxFuture<'a, Result<GroupInfo, P2pError>> {
        Box::pin(async move {
            let dev = self.device().await?;
            let peer = self
                .api
                .peers(&dev)
                .await?
                .into_iter()
                .find(|p| p.path == peer_id)
                .ok_or_else(|| P2pError::PeerNotFound(peer_id.to_string()))?;
            let spec = P2pConnectionSpec {
                id: format!("mirrorwire-miracast-{}", peer.hw_address),
                peer: peer.hw_address,
                wps,
                wfd_ies: self.ies(),
            };
            self.api.connect(&dev, &spec).await
        })
    }

    fn disconnect(&self) -> BoxFuture<'_, Result<(), P2pError>> {
        Box::pin(async move {
            let dev = self.device().await?;
            self.api.disconnect(&dev).await
        })
    }

    fn next_event(&self) -> BoxFuture<'_, Option<P2pEvent>> {
        self.api.next_event()
    }
}

/// Recording [`NmApi`] used by this module's tests and available to other crates' tests.
#[derive(Default)]
pub struct MockNm {
    pub devices: Vec<String>,
    pub peers: Vec<NmPeer>,
    pub group: Option<GroupInfo>,
    calls: Mutex<Vec<String>>,
    events: Mutex<VecDeque<P2pEvent>>,
    specs: Mutex<Vec<P2pConnectionSpec>>,
}

impl MockNm {
    fn log(&self, s: String) {
        self.calls.lock().unwrap_or_else(|e| e.into_inner()).push(s);
    }
    pub fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
    pub fn specs(&self) -> Vec<P2pConnectionSpec> {
        self.specs.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
    pub fn push_event(&self, e: P2pEvent) {
        self.events
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push_back(e);
    }
}

impl NmApi for MockNm {
    fn p2p_devices(&self) -> BoxFuture<'_, Result<Vec<String>, P2pError>> {
        Box::pin(async { Ok(self.devices.clone()) })
    }
    fn start_find<'a>(
        &'a self,
        device: &'a str,
        timeout: Duration,
    ) -> BoxFuture<'a, Result<(), P2pError>> {
        self.log(format!("StartFind {device} {}s", timeout.as_secs()));
        Box::pin(async { Ok(()) })
    }
    fn stop_find<'a>(&'a self, device: &'a str) -> BoxFuture<'a, Result<(), P2pError>> {
        self.log(format!("StopFind {device}"));
        Box::pin(async { Ok(()) })
    }
    fn peers<'a>(&'a self, _device: &'a str) -> BoxFuture<'a, Result<Vec<NmPeer>, P2pError>> {
        Box::pin(async { Ok(self.peers.clone()) })
    }
    fn set_wfd_ies<'a>(
        &'a self,
        device: &'a str,
        ies: &'a [u8],
    ) -> BoxFuture<'a, Result<(), P2pError>> {
        self.log(format!("SetWfdIes {device} {}", ies.len()));
        Box::pin(async { Ok(()) })
    }
    fn connect<'a>(
        &'a self,
        device: &'a str,
        spec: &'a P2pConnectionSpec,
    ) -> BoxFuture<'a, Result<GroupInfo, P2pError>> {
        self.log(format!("AddAndActivate {device} peer={}", spec.peer));
        self.specs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(spec.clone());
        let g = self
            .group
            .clone()
            .ok_or_else(|| P2pError::Timeout("no group formed".into()));
        Box::pin(async { g })
    }
    fn disconnect<'a>(&'a self, device: &'a str) -> BoxFuture<'a, Result<(), P2pError>> {
        self.log(format!("Deactivate {device}"));
        Box::pin(async { Ok(()) })
    }
    fn next_event(&self) -> BoxFuture<'_, Option<P2pEvent>> {
        let e = self
            .events
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pop_front();
        Box::pin(async { e })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::GroupRole;
    use std::net::Ipv4Addr;

    fn peer() -> NmPeer {
        NmPeer {
            path: "/org/freedesktop/NetworkManager/WifiP2PPeer/3".into(),
            name: "TB336FU".into(),
            hw_address: "02:11:22:33:44:55".into(),
            wfd_ies: vec![],
            strength: 80,
        }
    }

    fn provider() -> NmP2pProvider<MockNm> {
        NmP2pProvider::new(MockNm {
            devices: vec!["/org/freedesktop/NetworkManager/Devices/4".into()],
            peers: vec![peer()],
            group: Some(GroupInfo {
                interface: "p2p-wlp3s0-0".into(),
                role: GroupRole::Client,
                local_ip: Ipv4Addr::new(192, 168, 49, 2),
                peer_ip: Some(Ipv4Addr::new(192, 168, 49, 1)),
            }),
            ..MockNm::default()
        })
    }

    #[tokio::test]
    async fn no_p2p_device_is_a_readable_error() {
        let p = NmP2pProvider::new(MockNm::default());
        let err = p.start_find().await.unwrap_err();
        assert!(matches!(err, P2pError::NotAvailable(_)));
        assert!(err.to_string().contains("no Wi-Fi P2P device"));
    }

    #[tokio::test]
    async fn find_and_stop_use_the_first_device() {
        let p = provider();
        p.start_find().await.unwrap();
        p.stop_find().await.unwrap();
        assert_eq!(
            p.api.calls(),
            [
                "StartFind /org/freedesktop/NetworkManager/Devices/4 120s",
                "StopFind /org/freedesktop/NetworkManager/Devices/4"
            ]
        );
    }

    #[tokio::test]
    async fn connect_builds_a_pbc_profile_with_our_wfd_ies() {
        let p = provider();
        let ies = crate::wfd::sink_device_info(7236, 50);
        p.set_wfd_ies(&ies).await.unwrap();
        let g = p.connect(&peer().path, WpsMethod::Pbc).await.unwrap();
        assert_eq!(g.interface, "p2p-wlp3s0-0");
        let specs = p.api.specs();
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].peer, "02:11:22:33:44:55");
        assert_eq!(specs[0].wps_method_value(), 0x4);
        assert_eq!(specs[0].wfd_ies, ies);
    }

    #[tokio::test]
    async fn connect_to_unknown_peer_fails_without_touching_nm() {
        let p = provider();
        let err = p.connect("/nope", WpsMethod::Pbc).await.unwrap_err();
        assert!(matches!(err, P2pError::PeerNotFound(_)));
        assert!(p.api.specs().is_empty());
    }

    #[tokio::test]
    async fn events_pass_through() {
        let p = provider();
        p.api.push_event(P2pEvent::GroupEnded);
        assert_eq!(p.next_event().await, Some(P2pEvent::GroupEnded));
        assert_eq!(p.next_event().await, None);
    }
}
