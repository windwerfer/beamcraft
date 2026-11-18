//! Connection details travel as a `mirrorwire://host:port?fp=..&code=..&name=..`
//! URI, shown as a QR code by the presenter and scanned or pasted by the viewer.

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectInfo {
    pub addr: SocketAddr,
    /// Extra candidate addresses from mDNS, tried after `addr`.
    pub addrs: Vec<SocketAddr>,
    pub fingerprint: String,
    pub pairing_code: Option<String>,
    pub name: Option<String>,
}

impl ConnectInfo {
    /// Unique addresses to try, `addr` first, then `addrs` in order.
    pub fn candidates(&self) -> Vec<SocketAddr> {
        let mut out = Vec::with_capacity(1 + self.addrs.len());
        out.push(self.addr);
        for a in &self.addrs {
            if !out.contains(a) {
                out.push(*a);
            }
        }
        out
    }

    pub fn to_uri(&self) -> String {
        let Ok(mut url) = url::Url::parse(&format!("mirrorwire://{}", self.addr)) else {
            // Cannot happen for a SocketAddr; the bare form is still a valid link.
            return format!("mirrorwire://{}?fp={}", self.addr, self.fingerprint);
        };
        {
            let mut q = url.query_pairs_mut();
            q.append_pair("fp", &self.fingerprint);
            if let Some(code) = &self.pairing_code {
                q.append_pair("code", code);
            }
            if let Some(name) = &self.name {
                q.append_pair("name", name);
            }
        }
        url.to_string()
    }

    pub fn parse(uri: &str) -> Result<Self> {
        let url = url::Url::parse(uri.trim()).context("invalid mirrorwire URI")?;
        if url.scheme() != "mirrorwire" {
            bail!("expected mirrorwire:// URI");
        }
        let host = url.host_str().ok_or_else(|| anyhow!("URI has no host"))?;
        let ip: IpAddr = host.trim_matches(|c| c == '[' || c == ']').parse()?;
        let port = url.port().ok_or_else(|| anyhow!("URI has no port"))?;
        let params: BTreeMap<_, _> = url.query_pairs().into_owned().collect();
        let fingerprint = params
            .get("fp")
            .cloned()
            .ok_or_else(|| anyhow!("URI has no fp parameter"))?;
        if fingerprint.len() != 64 || !fingerprint.bytes().all(|b| b.is_ascii_hexdigit()) {
            bail!("fingerprint must be 64 hex characters");
        }
        Ok(Self {
            addr: SocketAddr::new(ip, port),
            addrs: Vec::new(),
            fingerprint,
            pairing_code: params.get("code").cloned(),
            name: params.get("name").cloned(),
        })
    }
}

pub fn qr_terminal(data: &str) -> Result<String> {
    let code = qrcode::QrCode::new(data.as_bytes())?;
    Ok(code
        .render::<qrcode::render::unicode::Dense1x2>()
        .quiet_zone(true)
        .build())
}

/// QR modules as a square boolean matrix (true = dark), for drawing in a GUI.
pub fn qr_matrix(data: &str) -> Result<(usize, Vec<bool>)> {
    let code = qrcode::QrCode::new(data.as_bytes())?;
    let width = code.width();
    let modules = code
        .to_colors()
        .into_iter()
        .map(|c| c == qrcode::Color::Dark)
        .collect();
    Ok((width, modules))
}

/// Largest camera frame (pixels) the QR scanner accepts; bigger input is rejected unread.
pub const QR_SCAN_MAX_PIXELS: usize = 16 * 1024 * 1024;

/// Looks for a `mirrorwire://` QR code in an 8-bit grayscale image (row-major, `width` bytes
/// per row, no padding), e.g. a camera frame's luma plane. Returns the parsed link of the first
/// QR code that holds one; other QR codes, damaged codes and bad sizes give `None`.
pub fn scan_qr_luma(width: usize, height: usize, luma: &[u8]) -> Option<ConnectInfo> {
    let pixels = width.checked_mul(height)?;
    if width == 0 || height == 0 || pixels > QR_SCAN_MAX_PIXELS || luma.len() < pixels {
        return None;
    }
    let mut img = rqrr::PreparedImage::prepare_from_greyscale(width, height, |x, y| {
        luma.get(y * width + x).copied().unwrap_or(0)
    });
    img.detect_grids()
        .into_iter()
        .filter_map(|grid| grid.decode().ok())
        .find_map(|(_, content)| ConnectInfo::parse(&content).ok())
}

pub fn random_pairing_code() -> Result<String> {
    let mut buf = [0u8; 4];
    getrandom::fill(&mut buf).map_err(|e| anyhow!("random: {e}"))?;
    Ok(format!("{:06}", u32::from_le_bytes(buf) % 1_000_000))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PairedViewer {
    pub fingerprint: String,
    pub name: String,
}

/// Viewers that have presented a valid pairing code once and are trusted since.
pub struct PairedViewers {
    path: PathBuf,
    entries: Vec<PairedViewer>,
}

impl PairedViewers {
    pub fn load(dir: &Path) -> Self {
        let path = dir.join("paired_viewers.json");
        let entries = std::fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        Self { path, entries }
    }

    pub fn contains(&self, fingerprint: &str) -> bool {
        self.entries.iter().any(|e| e.fingerprint == fingerprint)
    }

    pub fn add(&mut self, fingerprint: &str, name: &str) -> Result<()> {
        if self.contains(fingerprint) {
            return Ok(());
        }
        self.entries.push(PairedViewer {
            fingerprint: fingerprint.to_owned(),
            name: name.to_owned(),
        });
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(&self.path, serde_json::to_vec_pretty(&self.entries)?)?;
        Ok(())
    }
}

/// Non-loopback IPv4 addresses, global (or site) first, then link-local.
pub fn lan_ipv4s() -> Vec<IpAddr> {
    let mut ips: Vec<IpAddr> = if_addrs::get_if_addrs()
        .ok()
        .into_iter()
        .flatten()
        .filter(|i| !i.is_loopback())
        .map(|i| i.ip())
        .filter(IpAddr::is_ipv4)
        .collect();
    ips.sort_by_key(|ip| match ip {
        IpAddr::V4(v) if v.is_link_local() => 1,
        IpAddr::V4(_) => 0,
        _ => 2,
    });
    ips.dedup();
    ips
}

pub fn primary_ipv4() -> Option<IpAddr> {
    lan_ipv4s().into_iter().next()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Renders `data` as a QR code into a grayscale image, `scale` pixels per module, with a
    /// 4-module quiet zone.
    fn render_luma(data: &str, scale: usize) -> (usize, usize, Vec<u8>) {
        let (n, modules) = qr_matrix(data).unwrap();
        let side = (n + 8) * scale;
        let mut img = vec![255u8; side * side];
        for (i, dark) in modules.iter().enumerate() {
            if !dark {
                continue;
            }
            let (mx, my) = (i % n + 4, i / n + 4);
            for y in 0..scale {
                for x in 0..scale {
                    img[(my * scale + y) * side + mx * scale + x] = 0;
                }
            }
        }
        (side, side, img)
    }

    fn sample() -> ConnectInfo {
        ConnectInfo {
            addr: "192.168.1.20:47800".parse().unwrap(),
            addrs: vec![],
            fingerprint: "ab".repeat(32),
            pairing_code: Some("123456".into()),
            name: Some("Laptop".into()),
        }
    }

    #[test]
    fn scan_finds_the_presenter_qr() {
        let info = sample();
        let (w, h, img) = render_luma(&info.to_uri(), 4);
        assert_eq!(scan_qr_luma(w, h, &img), Some(info));
    }

    #[test]
    fn scan_ignores_other_qr_codes_and_noise() {
        let (w, h, img) = render_luma("https://example.org/", 4);
        assert_eq!(scan_qr_luma(w, h, &img), None);
        assert_eq!(scan_qr_luma(64, 64, &[128u8; 64 * 64]), None);
    }

    #[test]
    fn scan_rejects_bad_sizes_without_panicking() {
        assert_eq!(scan_qr_luma(0, 10, &[]), None);
        assert_eq!(scan_qr_luma(10, 10, &[0u8; 50]), None);
        assert_eq!(scan_qr_luma(usize::MAX, 2, &[0u8; 4]), None);
        assert_eq!(scan_qr_luma(1 << 20, 1 << 20, &[]), None);
    }

    #[test]
    fn uri_roundtrip() {
        let info = ConnectInfo {
            addr: "192.168.1.20:47800".parse().unwrap(),
            addrs: Vec::new(),
            fingerprint: "ab".repeat(32),
            pairing_code: Some("012345".into()),
            name: Some("T480s laptop".into()),
        };
        assert_eq!(info.candidates(), vec![info.addr]);
        let uri = info.to_uri();
        assert!(uri.starts_with("mirrorwire://192.168.1.20:47800?"));
        assert_eq!(ConnectInfo::parse(&uri).unwrap(), info);
        assert!(qr_terminal(&uri).unwrap().lines().count() > 10);
    }

    #[test]
    fn uri_roundtrip_custom_port() {
        let info = ConnectInfo {
            addr: "10.0.0.8:47901".parse().unwrap(),
            addrs: Vec::new(),
            fingerprint: "cd".repeat(32),
            pairing_code: Some("654321".into()),
            name: Some("custom-port".into()),
        };
        let uri = info.to_uri();
        assert!(uri.starts_with("mirrorwire://10.0.0.8:47901?"), "{uri}");
        assert_eq!(ConnectInfo::parse(&uri).unwrap(), info);
    }

    #[test]
    fn rejects_bad_fingerprint() {
        assert!(ConnectInfo::parse("mirrorwire://127.0.0.1:1?fp=xyz").is_err());
    }

    #[test]
    fn lan_ipv4s_omit_loopback() {
        for ip in lan_ipv4s() {
            assert!(ip.is_ipv4(), "{ip}");
            assert!(!ip.is_loopback(), "{ip}");
        }
    }

    #[test]
    fn candidates_dedup_and_preserve_order() {
        let a: SocketAddr = "192.168.1.2:47800".parse().unwrap();
        let b: SocketAddr = "10.0.0.5:47800".parse().unwrap();
        let info = ConnectInfo {
            addr: a,
            addrs: vec![a, b, a],
            fingerprint: "ab".repeat(32),
            pairing_code: None,
            name: None,
        };
        assert_eq!(info.candidates(), vec![a, b]);
    }
}

#[cfg(test)]
mod hostile_tests {
    use super::*;

    #[test]
    fn uri_parser_rejects_junk_without_panicking() {
        let mut x = 0x2545_f491_4f6c_dd1du64;
        let fp = "a".repeat(64);
        let valid = format!("mirrorwire://192.168.1.2:47800?fp={fp}&code=123456&name=x");
        assert!(ConnectInfo::parse(&valid).is_ok());
        for _ in 0..5_000 {
            let mut s: Vec<u8> = valid.bytes().collect();
            for _ in 0..4 {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                let i = (x % s.len() as u64) as usize;
                match x % 3 {
                    0 => s[i] = (x >> 8) as u8,
                    1 => s.truncate(i),
                    _ => s.insert(i, (x >> 16) as u8),
                }
                if s.is_empty() {
                    break;
                }
            }
            let _ = ConnectInfo::parse(&String::from_utf8_lossy(&s));
        }
        for bad in [
            "",
            "mirrorwire://",
            "mirrorwire://[::1]",
            "mirrorwire://[::1]:99999?fp=1",
        ] {
            assert!(ConnectInfo::parse(bad).is_err(), "{bad}");
        }
    }
}
