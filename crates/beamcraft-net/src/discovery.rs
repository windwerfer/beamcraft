use crate::pairing::lan_ipv4s;
use anyhow::Result;
use beamcraft_proto::{MDNS_SERVICE, PROTOCOL_VERSION};
use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};
use std::net::{IpAddr, SocketAddr};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Presenter {
    pub name: String,
    /// Preferred address (first LAN IPv4).
    pub addr: SocketAddr,
    /// Every IPv4 advertised for this presenter, preferred first.
    pub addrs: Vec<SocketAddr>,
    pub fingerprint: String,
    pub resolution: Option<String>,
    /// TXT `v`: the presenter's `PROTOCOL_VERSION`. `None` if missing or not a number.
    pub protocol: Option<u32>,
}

impl Presenter {
    /// Why this presenter can't be used from this build, for the viewer's list; `None` if it
    /// speaks our protocol.
    pub fn version_problem(&self) -> Option<String> {
        version_problem(self.protocol)
    }
}

fn version_problem(protocol: Option<u32>) -> Option<String> {
    match protocol {
        Some(v) if v == PROTOCOL_VERSION => None,
        Some(v) if v > PROTOCOL_VERSION => Some(format!(
            "newer BeamCraft (protocol {v}, this viewer {PROTOCOL_VERSION}): update this viewer"
        )),
        Some(v) => Some(format!(
            "older BeamCraft (protocol {v}, this viewer {PROTOCOL_VERSION}): update the presenter"
        )),
        None => Some("older BeamCraft (no protocol version): update the presenter".to_owned()),
    }
}

/// TXT `v` from outside: a decimal `u32` or nothing.
fn parse_protocol(txt: Option<&str>) -> Option<u32> {
    txt?.trim().parse().ok()
}

/// Keeps a presenter announced on the local network while alive.
pub struct Advertisement {
    daemon: ServiceDaemon,
    fullname: String,
}

impl Advertisement {
    pub fn start(name: &str, port: u16, fingerprint: &str, resolution: &str) -> Result<Self> {
        let daemon = ServiceDaemon::new()?;
        let host = hostname::get()
            .ok()
            .and_then(|h| h.into_string().ok())
            .unwrap_or_else(|| "beamcraft".into());
        let version = PROTOCOL_VERSION.to_string();
        let props = [("fp", fingerprint), ("v", &version), ("res", resolution)];
        let ips = lan_ipv4s();
        let mut info = if ips.is_empty() {
            ServiceInfo::new(
                MDNS_SERVICE,
                name,
                &format!("{host}.local."),
                (),
                port,
                &props[..],
            )?
        } else {
            tracing::info!(
                "mDNS advertising {} on UDP port {port}",
                ips.iter()
                    .map(|ip| ip.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            ServiceInfo::new(
                MDNS_SERVICE,
                name,
                &format!("{host}.local."),
                &ips[..],
                port,
                &props[..],
            )?
        };
        info = info.enable_addr_auto();
        let fullname = info.get_fullname().to_owned();
        daemon.register(info)?;
        Ok(Self { daemon, fullname })
    }
}

impl Drop for Advertisement {
    fn drop(&mut self) {
        let _ = self.daemon.unregister(&self.fullname);
        let _ = self.daemon.shutdown();
    }
}

/// Continuously browses for presenters; poll with `poll` from any loop.
pub struct Browser {
    daemon: ServiceDaemon,
    events: mdns_sd::Receiver<ServiceEvent>,
    found: Vec<Presenter>,
}

impl Browser {
    pub fn start() -> Result<Self> {
        let daemon = ServiceDaemon::new()?;
        let events = daemon.browse(MDNS_SERVICE)?;
        Ok(Self {
            daemon,
            events,
            found: Vec::new(),
        })
    }

    pub fn poll(&mut self) -> &[Presenter] {
        while let Ok(event) = self.events.try_recv() {
            self.apply(event);
        }
        &self.found
    }

    pub fn browse_for(mut self, wait: Duration) -> Vec<Presenter> {
        let deadline = Instant::now() + wait;
        while let Some(left) = deadline.checked_duration_since(Instant::now()) {
            match self.events.recv_timeout(left) {
                Ok(event) => self.apply(event),
                Err(_) => break,
            }
        }
        std::mem::take(&mut self.found)
    }

    fn apply(&mut self, event: ServiceEvent) {
        match event {
            ServiceEvent::ServiceResolved(info) => {
                let Some(fp) = info.get_property_val_str("fp") else {
                    return;
                };
                let port = info.get_port();
                let mut ips: Vec<IpAddr> = info
                    .get_addresses()
                    .iter()
                    .map(|a| a.to_ip_addr())
                    .collect();
                // Prefer non-loopback IPv4, then link-local IPv4, then anything else.
                ips.sort_by_key(|a| match a {
                    IpAddr::V4(v) if a.is_loopback() => 2,
                    IpAddr::V4(v) if v.is_link_local() => 1,
                    IpAddr::V4(_) => 0,
                    _ if a.is_loopback() => 4,
                    _ => 3,
                });
                ips.dedup();
                let addrs: Vec<SocketAddr> = ips
                    .iter()
                    .filter(|ip| ip.is_ipv4() && !ip.is_loopback())
                    .map(|ip| SocketAddr::new(*ip, port))
                    .collect();
                let addrs = if addrs.is_empty() {
                    ips.into_iter()
                        .map(|ip| SocketAddr::new(ip, port))
                        .collect()
                } else {
                    addrs
                };
                let Some(addr) = addrs.first().copied() else {
                    return;
                };
                let name = info
                    .get_fullname()
                    .trim_end_matches(MDNS_SERVICE)
                    .trim_end_matches('.')
                    .to_owned();
                tracing::info!(
                    "mDNS resolved {name} at {}",
                    addrs
                        .iter()
                        .map(|a| a.to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                );
                let presenter = Presenter {
                    name,
                    addr,
                    addrs,
                    fingerprint: fp.to_owned(),
                    resolution: info.get_property_val_str("res").map(str::to_owned),
                    protocol: parse_protocol(info.get_property_val_str("v")),
                };
                self.found
                    .retain(|p| p.fingerprint != presenter.fingerprint);
                self.found.push(presenter);
            }
            ServiceEvent::ServiceRemoved(_, fullname) => {
                let name = fullname
                    .trim_end_matches(MDNS_SERVICE)
                    .trim_end_matches('.');
                self.found.retain(|p| p.name != name);
            }
            _ => {}
        }
    }
}

impl Drop for Browser {
    fn drop(&mut self) {
        let _ = self.daemon.stop_browse(MDNS_SERVICE);
        let _ = self.daemon.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn txt_v_parses_only_a_plain_number() {
        assert_eq!(parse_protocol(Some("3")), Some(3));
        assert_eq!(parse_protocol(Some(" 3 ")), Some(3));
        assert_eq!(parse_protocol(None), None);
        for bad in ["", "x", "-1", "3.0", "99999999999"] {
            assert_eq!(parse_protocol(Some(bad)), None, "{bad:?}");
        }
    }

    #[test]
    fn only_another_protocol_needs_an_update() {
        assert_eq!(version_problem(Some(PROTOCOL_VERSION)), None);
        let newer = version_problem(Some(PROTOCOL_VERSION + 1)).unwrap();
        assert!(newer.contains("update this viewer"), "{newer}");
        let older = version_problem(Some(PROTOCOL_VERSION - 1)).unwrap();
        assert!(older.contains("update the presenter"), "{older}");
        assert!(version_problem(None)
            .unwrap()
            .contains("update the presenter"));
    }
}
