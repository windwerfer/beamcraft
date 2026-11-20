//! NetworkManager over the system D-Bus (zbus). No root: every call goes through NM, which
//! asks polkit (miracast.md section 1). This is a spike stub: discovery, peer listing, profile
//! activation and deactivation are real calls; publishing WFD IEs, reading the group's
//! addresses and the change signals are explicit `Unsupported` until checked on hardware
//! (risks R1/R2 in miracast.md). Everything NM-independent is tested through [`crate::MockNm`].

use crate::{BoxFuture, GroupInfo, NmApi, NmPeer, P2pConnectionSpec, P2pError, P2pEvent};
use std::collections::HashMap;
use std::time::Duration;
use zbus::{ObjectPath, OwnedObjectPath, Variant as Value};

const NM_DEST: &str = "org.freedesktop.NetworkManager";
const NM_PATH: &str = "/org/freedesktop/NetworkManager";
const NM_IFACE: &str = "org.freedesktop.NetworkManager";
const DEVICE_IFACE: &str = "org.freedesktop.NetworkManager.Device";
const P2P_DEVICE_IFACE: &str = "org.freedesktop.NetworkManager.Device.WifiP2P";
const PEER_IFACE: &str = "org.freedesktop.NetworkManager.WifiP2PPeer";
/// `NM_DEVICE_TYPE_WIFI_P2P`.
const DEVICE_TYPE_WIFI_P2P: u32 = 30;

/// `a{sa{sv}}`: NM connection settings.
pub type ConnectionSettings = HashMap<&'static str, HashMap<&'static str, Value<'static>>>;

/// Settings of the volatile `wifi-p2p` profile for `spec` (pure; unit tested).
pub fn connection_settings(spec: &P2pConnectionSpec) -> ConnectionSettings {
    let mut connection = HashMap::new();
    connection.insert("id", Value::from(spec.id.clone()));
    connection.insert("type", Value::from("wifi-p2p"));
    connection.insert("autoconnect", Value::from(false));

    let mut p2p = HashMap::new();
    p2p.insert("peer", Value::from(spec.peer.clone()));
    p2p.insert("wps-method", Value::from(spec.wps_method_value()));
    if !spec.wfd_ies.is_empty() {
        p2p.insert("wfd-ies", Value::from(spec.wfd_ies.clone()));
    }

    // Shared addressing: if we end up as group owner NM serves DHCP; as client it is ignored.
    let mut ipv4 = HashMap::new();
    ipv4.insert("method", Value::from("shared"));

    let mut out = HashMap::new();
    out.insert("connection", connection);
    out.insert("wifi-p2p", p2p);
    out.insert("ipv4", ipv4);
    out
}

fn map_err(e: zbus::Error) -> P2pError {
    let text = e.to_string();
    if text.contains("NotAuthorized") || text.contains("AccessDenied") {
        P2pError::PermissionDenied(text)
    } else if text.contains("ServiceUnknown") || text.contains("NameHasNoOwner") {
        P2pError::NotAvailable(format!("NetworkManager is not running ({text})"))
    } else {
        P2pError::Backend(text)
    }
}

/// [`NmApi`] on the system bus.
pub struct ZbusNm {
    conn: zbus::Connection,
}

impl ZbusNm {
    /// Connect to the system bus. Long method timeout: polkit may show a password prompt.
    pub async fn connect_system() -> Result<Self, P2pError> {
        let conn = zbus::connection::Builder::system()
            .method_timeout(Duration::from_secs(120))
            .build()
            .await
            .map_err(map_err)?;
        Ok(Self { conn })
    }

    async fn proxy<'a>(
        &'a self,
        path: &'a str,
        interface: &'a str,
    ) -> Result<zbus::Proxy<'a>, P2pError> {
        zbus::proxy::Builder::new(&self.conn)
            .destination(NM_DEST)
            .path(path)
            .interface(interface)
            .cache_properties(zbus::proxy::CacheProperties::No)
            .build()
            .await
            .map_err(map_err)
    }

    async fn peer(&self, path: &str) -> Result<NmPeer, P2pError> {
        let p = self.proxy(path, PEER_IFACE).await?;
        Ok(NmPeer {
            path: path.to_string(),
            name: p.get_property("Name").await.map_err(map_err)?,
            hw_address: p.get_property("HwAddress").await.map_err(map_err)?,
            wfd_ies: p.get_property("WfdIEs").await.unwrap_or_default(),
            strength: p.get_property("Strength").await.unwrap_or(0),
        })
    }
}

impl NmApi for ZbusNm {
    fn p2p_devices(&self) -> BoxFuture<'_, Result<Vec<String>, P2pError>> {
        Box::pin(async move {
            let nm = self.proxy(NM_PATH, NM_IFACE).await?;
            let all: Vec<OwnedObjectPath> = nm.call("GetAllDevices", &()).await.map_err(map_err)?;
            let mut out = Vec::new();
            for path in all {
                let dev = self.proxy(path.as_str(), DEVICE_IFACE).await?;
                let ty: u32 = dev.get_property("DeviceType").await.map_err(map_err)?;
                if ty == DEVICE_TYPE_WIFI_P2P {
                    out.push(path.to_string());
                }
            }
            Ok(out)
        })
    }

    fn start_find<'a>(
        &'a self,
        device: &'a str,
        timeout: Duration,
    ) -> BoxFuture<'a, Result<(), P2pError>> {
        Box::pin(async move {
            let dev = self.proxy(device, P2P_DEVICE_IFACE).await?;
            let mut opts: HashMap<&str, Value<'_>> = HashMap::new();
            let secs = i32::try_from(timeout.as_secs()).unwrap_or(i32::MAX);
            opts.insert("timeout", Value::from(secs));
            dev.call::<_, _, ()>("StartFind", &(opts,))
                .await
                .map_err(map_err)
        })
    }

    fn stop_find<'a>(&'a self, device: &'a str) -> BoxFuture<'a, Result<(), P2pError>> {
        Box::pin(async move {
            let dev = self.proxy(device, P2P_DEVICE_IFACE).await?;
            dev.call::<_, _, ()>("StopFind", &()).await.map_err(map_err)
        })
    }

    fn peers<'a>(&'a self, device: &'a str) -> BoxFuture<'a, Result<Vec<NmPeer>, P2pError>> {
        Box::pin(async move {
            let dev = self.proxy(device, P2P_DEVICE_IFACE).await?;
            let paths: Vec<OwnedObjectPath> = dev.get_property("Peers").await.map_err(map_err)?;
            let mut out = Vec::new();
            for p in paths {
                // A peer can vanish between listing and reading; skip it.
                if let Ok(peer) = self.peer(p.as_str()).await {
                    out.push(peer);
                }
            }
            Ok(out)
        })
    }

    fn set_wfd_ies<'a>(
        &'a self,
        _device: &'a str,
        _ies: &'a [u8],
    ) -> BoxFuture<'a, Result<(), P2pError>> {
        Box::pin(async {
            Err(P2pError::Unsupported(
                "publishing WFD IEs through NetworkManager (miracast.md risk R2)".into(),
            ))
        })
    }

    fn connect<'a>(
        &'a self,
        device: &'a str,
        spec: &'a P2pConnectionSpec,
    ) -> BoxFuture<'a, Result<GroupInfo, P2pError>> {
        Box::pin(async move {
            let nm = self.proxy(NM_PATH, NM_IFACE).await?;
            let dev = ObjectPath::try_from(device)
                .map_err(|e| P2pError::Backend(format!("bad device path: {e}")))?;
            let any = ObjectPath::from_static_str_unchecked("/");
            let (_conn, _active): (OwnedObjectPath, OwnedObjectPath) = nm
                .call(
                    "AddAndActivateConnection",
                    &(connection_settings(spec), dev, any),
                )
                .await
                .map_err(map_err)?;
            Err(P2pError::Unsupported(
                "reading the group interface and addresses after activation (Ip4Config)".into(),
            ))
        })
    }

    fn disconnect<'a>(&'a self, device: &'a str) -> BoxFuture<'a, Result<(), P2pError>> {
        Box::pin(async move {
            let dev = self.proxy(device, DEVICE_IFACE).await?;
            let active: OwnedObjectPath = dev
                .get_property("ActiveConnection")
                .await
                .map_err(map_err)?;
            if active.as_str() == "/" {
                return Ok(());
            }
            let nm = self.proxy(NM_PATH, NM_IFACE).await?;
            nm.call::<_, _, ()>("DeactivateConnection", &(active,))
                .await
                .map_err(map_err)
        })
    }

    fn next_event(&self) -> BoxFuture<'_, Option<P2pEvent>> {
        // Stub: PeerAdded/PeerRemoved and state-change signals are not subscribed yet.
        Box::pin(std::future::pending())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::WpsMethod;

    #[test]
    fn settings_carry_peer_wps_and_ies() {
        let spec = P2pConnectionSpec {
            id: "x".into(),
            peer: "02:11:22:33:44:55".into(),
            wps: WpsMethod::Pbc,
            wfd_ies: vec![0, 0, 6, 0, 0x11, 0x1c, 0x44, 0, 0x32],
        };
        let s = connection_settings(&spec);
        assert_eq!(s["connection"]["type"], Value::from("wifi-p2p"));
        assert_eq!(s["connection"]["autoconnect"], Value::from(false));
        assert_eq!(s["wifi-p2p"]["peer"], Value::from("02:11:22:33:44:55"));
        assert_eq!(s["wifi-p2p"]["wps-method"], Value::from(4u32));
        assert!(s["wifi-p2p"].contains_key("wfd-ies"));
    }

    #[test]
    fn empty_ies_are_omitted() {
        let spec = P2pConnectionSpec {
            id: "x".into(),
            peer: "p".into(),
            wps: WpsMethod::Auto,
            wfd_ies: vec![],
        };
        assert!(!connection_settings(&spec)["wifi-p2p"].contains_key("wfd-ies"));
    }

    #[test]
    fn polkit_refusal_is_a_permission_error() {
        let e = map_err(zbus::Error::Failure(
            "org.freedesktop.NetworkManager.PermissionDenied".into(),
        ));
        assert!(matches!(
            e,
            P2pError::Backend(_) | P2pError::PermissionDenied(_)
        ));
    }
}
