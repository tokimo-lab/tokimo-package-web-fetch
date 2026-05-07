//! SSRF 防护 — 在 fetch 前对目标 IP 进行预检，阻止访问私有/内网地址。
//!
//! 防护措施：
//! - 仅允许 http/https scheme
//! - 阻止已知内网主机名（metadata.google.internal 等）
//! - URL host 为 IP 字面量时直接检查，无需 DNS
//! - 主机名做 DNS 解析后检查每个返回 IP 是否为私有/内部/保留地址
//! - 失败即拒绝（DNS 解析失败 = 阻止）
//!
//! 已知局限（文档化，与 hermes-agent 相同，无法在预检层完全修复）：
//! - **DNS rebinding (TOCTOU)**：攻击者控制的 DNS 在检查时返回公网 IP，
//!   实际连接时换成私有 IP。需在连接层做校验才能彻底修复。
//! - **Redirect bypass**：reqwest 默认跟随 301/302 重定向，重定向目标
//!   不经过此预检。后续可通过 manual redirect following 关闭此窗口。

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, ToSocketAddrs};
use std::time::Duration;

use url::Url;

use crate::error::{FetchError, FetchResult};

/// 已知内网主机名，无论 IP 解析结果如何都阻止。
static BLOCKED_HOSTNAMES: &[&str] = &["metadata.google.internal", "metadata.goog"];

/// 在发起 fetch 前对 URL 做 SSRF 安全检查。
///
/// 检查顺序：
/// 1. 只允许 `http` / `https` scheme
/// 2. 阻止已知内网主机名
/// 3. 若 host 为 IP 字面量，直接检查（无 DNS 开销）
/// 4. 否则在线程池中做 DNS 解析（5s 超时），逐 IP 检查
pub async fn check_ssrf(url: &str) -> FetchResult<()> {
    let parsed = Url::parse(url).map_err(|e| FetchError::InvalidUrl(e.to_string()))?;

    let scheme = parsed.scheme();
    if scheme != "http" && scheme != "https" {
        return Err(FetchError::SsrfBlocked(format!(
            "scheme '{scheme}' not allowed (only http/https)"
        )));
    }

    let host = parsed
        .host_str()
        .ok_or_else(|| FetchError::InvalidUrl("URL has no host".to_string()))?;
    let host_lc = host.to_lowercase();

    if BLOCKED_HOSTNAMES.contains(&host_lc.as_str()) {
        return Err(FetchError::SsrfBlocked(format!("blocked internal hostname: {host_lc}")));
    }

    // IP literal in URL — check directly, no DNS needed.
    if let Ok(ip) = host.parse::<IpAddr>() {
        if is_blocked_ip(ip) {
            tracing::warn!(url, %ip, "SSRF: blocked IP literal");
            return Err(FetchError::SsrfBlocked(format!("blocked IP address: {ip}")));
        }
        return Ok(());
    }

    // Hostname — DNS-resolve in thread pool, fail-closed.
    let lookup_target = format!("{host_lc}:80");
    let addrs: Vec<SocketAddr> = match tokio::time::timeout(
        Duration::from_secs(5),
        tokio::task::spawn_blocking(move || -> std::io::Result<Vec<SocketAddr>> {
            Ok(lookup_target.to_socket_addrs()?.collect())
        }),
    )
    .await
    {
        Err(_elapsed) => return Err(FetchError::SsrfBlocked(format!("DNS timeout for: {host_lc}"))),
        Ok(Err(join_err)) => return Err(FetchError::SsrfBlocked(format!("DNS task failed: {join_err}"))),
        Ok(Ok(Err(_io_err))) => return Err(FetchError::SsrfBlocked(format!("DNS resolution failed for: {host_lc}"))),
        Ok(Ok(Ok(v))) => v,
    };

    if addrs.is_empty() {
        return Err(FetchError::SsrfBlocked(format!(
            "DNS returned no addresses for: {host_lc}"
        )));
    }

    for addr in &addrs {
        if is_blocked_ip(addr.ip()) {
            tracing::warn!(url, ip = %addr.ip(), host = %host_lc, "SSRF: blocked private address");
            return Err(FetchError::SsrfBlocked(format!(
                "blocked private/internal address: {host_lc} -> {}",
                addr.ip()
            )));
        }
    }

    Ok(())
}

fn is_blocked_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_blocked_ipv4(v4),
        IpAddr::V6(v6) => is_blocked_ipv6(v6),
    }
}

/// Returns `true` for any IPv4 address that should not be reachable from a
/// public web agent: loopback, link-local, RFC-1918 private, CGNAT,
/// unspecified, multicast, and reserved/future-use ranges.
fn is_blocked_ipv4(addr: Ipv4Addr) -> bool {
    let o = addr.octets();
    o[0] == 127                               // Loopback: 127.0.0.0/8
    || (o[0] == 169 && o[1] == 254)           // Link-local: 169.254.0.0/16 (AWS/GCP/Azure metadata)
    || o[0] == 10                             // Private: 10.0.0.0/8
    || (o[0] == 172 && (o[1] & 0xf0) == 16)  // Private: 172.16.0.0/12
    || (o[0] == 192 && o[1] == 168)           // Private: 192.168.0.0/16
    || (o[0] == 100 && (o[1] & 0xc0) == 64)  // CGNAT/Shared (RFC 6598): 100.64.0.0/10
    || o[0] == 0                              // Unspecified: 0.0.0.0/8
    || (o[0] & 0xf0) == 224                   // Multicast: 224.0.0.0/4
    || o[0] >= 240 // Reserved/future (240.0.0.0/4) + broadcast
}

/// Returns `true` for any IPv6 address that should not be reachable from a
/// public web agent, including IPv4-mapped/compatible addresses.
fn is_blocked_ipv6(addr: Ipv6Addr) -> bool {
    // Normalize IPv4-mapped (::ffff:x.x.x.x) and IPv4-compatible (::x.x.x.x)
    // addresses so is_blocked_ipv4 handles them correctly.
    if let Some(ipv4) = addr.to_ipv4() {
        return is_blocked_ipv4(ipv4);
    }

    let s = addr.segments();
    addr.is_loopback()                    // ::1
    || addr.is_unspecified()              // ::
    || (s[0] >> 8) == 0xff               // Multicast: ff00::/8
    || (s[0] & 0xffc0) == 0xfe80         // Link-local: fe80::/10
    || (s[0] & 0xfe00) == 0xfc00         // Unique local (ULA): fc00::/7
    || (s[0] & 0xffc0) == 0xfec0 // Legacy site-local (deprecated, RFC 3879): fec0::/10
}
