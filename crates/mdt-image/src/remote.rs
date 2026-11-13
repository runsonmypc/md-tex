//! Remote image fetching: `http` and `https` only, at most
//! `MAX_REDIRECTS` redirects that stay on those schemes, bodies capped at `MAX_BYTES`, no
//! cookies, credentials or referrer, and a generic User-Agent. Only public addresses are
//! connected to: a document must not make the reader send requests to this machine or its
//! local network (a router's admin page, a service on localhost). Proxy settings from the
//! environment are ignored: a proxy resolves the target itself, past that check.

use std::net::{IpAddr, SocketAddr};
use std::time::{Duration, Instant};

use ureq::config::Config;
use ureq::http::Uri;
use ureq::unversioned::resolver::{DefaultResolver, ResolvedSocketAddrs, Resolver};
use ureq::unversioned::transport::{DefaultConnector, NextTimeout};

use crate::ImageError;

pub const MAX_REDIRECTS: usize = 5;
pub const MAX_BYTES: u64 = 25 * 1024 * 1024;
pub const USER_AGENT: &str = "Mozilla/5.0";
/// The whole fetch, redirects included. Each request had a minute of its own, so a server
/// answering every redirect just in time held a fetch for six.
pub const TIMEOUT: Duration = Duration::from_secs(60);

fn agent(public_only: bool) -> ureq::Agent {
    let config = ureq::Agent::config_builder()
        // Redirects are followed by hand so each target's scheme can be checked.
        .max_redirects(0)
        .http_status_as_error(false)
        .user_agent(USER_AGENT)
        .timeout_global(Some(TIMEOUT))
        .proxy(None)
        .build();
    ureq::Agent::with_parts(
        config,
        DefaultConnector::default(),
        PublicResolver { public_only },
    )
}

/// Whether `ip` is on the public internet: not loopback, private, link-local, shared
/// (carrier-grade NAT), unspecified, broadcast, multicast, documentation or reserved
/// space. An IPv6 address that carries an IPv4 one (mapped, compatible, NAT64 or 6to4,
/// which gateways translate back to IPv4) is judged by that address.
pub(crate) fn is_public(ip: IpAddr) -> bool {
    let ip = match ip {
        IpAddr::V6(v6) => match v6.segments() {
            // NAT64 well-known prefix (64:ff9b::/96) and IPv4-compatible (::a.b.c.d).
            [0x64, 0xff9b, 0, 0, 0, 0, ..] | [0, 0, 0, 0, 0, 0, ..] => {
                IpAddr::V4(std::net::Ipv4Addr::from_bits(v6.to_bits() as u32))
            }
            // 6to4 (2002:a.b.c.d::/48).
            [0x2002, hi, lo, ..] => {
                IpAddr::V4(std::net::Ipv4Addr::from_bits((hi as u32) << 16 | lo as u32))
            }
            _ => ip.to_canonical(),
        },
        IpAddr::V4(_) => ip,
    };
    match ip {
        IpAddr::V4(v4) => {
            let [a, b, ..] = v4.octets();
            !(v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || v4.is_multicast()
                || v4.is_documentation()
                || a == 0
                || a >= 240
                || (a == 100 && (64..128).contains(&b))
                // Benchmarking (198.18.0.0/15) and IETF protocol space (192.0.0.0/24).
                || (a == 198 && (18..20).contains(&b))
                || (a == 192 && b == 0 && v4.octets()[2] == 0))
        }
        IpAddr::V6(v6) => {
            let first = v6.segments()[0];
            !(v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || v6.is_unique_local()
                || v6.is_unicast_link_local()
                // Local-use NAT64 (64:ff9b:1::/48).
                || v6.segments()[..3] == [0x64, 0xff9b, 1]
                // Deprecated site-local addresses.
                || first & 0xffc0 == 0xfec0)
        }
    }
}

/// Resolves names as usual, then keeps only public addresses, so every connection (the
/// first request and each redirect) goes to the address that was checked.
#[derive(Debug)]
struct PublicResolver {
    /// Off only in tests, whose servers listen on localhost.
    public_only: bool,
}

impl Resolver for PublicResolver {
    fn resolve(
        &self,
        uri: &Uri,
        config: &Config,
        timeout: NextTimeout,
    ) -> Result<ResolvedSocketAddrs, ureq::Error> {
        let all = DefaultResolver::default().resolve(uri, config, timeout)?;
        if !self.public_only {
            return Ok(all);
        }
        let mut public = self.empty();
        for addr in all.iter().filter(|a: &&SocketAddr| is_public(a.ip())) {
            public.push(*addr);
        }
        if public.is_empty() {
            return Err(ureq::Error::Io(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                format!("{} is not a public address", all[0].ip()),
            )));
        }
        Ok(public)
    }
}

fn web_url(s: &str) -> Result<url::Url, ImageError> {
    let u = url::Url::parse(s).map_err(|e| ImageError::Fetch(e.to_string()))?;
    match u.scheme() {
        "http" | "https" if u.username().is_empty() && u.password().is_none() => Ok(u),
        "http" | "https" => Err(ImageError::Fetch("credentials in URL".into())),
        other => Err(ImageError::Fetch(format!("unsupported scheme {other}"))),
    }
}

/// Fetches `url`'s body from a public address.
pub fn fetch(url: &str) -> Result<Vec<u8>, ImageError> {
    fetch_from(url, true)
}

/// `fetch`, optionally allowing any address (tests).
pub(crate) fn fetch_from(url: &str, public_only: bool) -> Result<Vec<u8>, ImageError> {
    fetch_within(url, public_only, TIMEOUT)
}

/// `fetch_from`, given up after `timeout` in all.
pub(crate) fn fetch_within(
    url: &str,
    public_only: bool,
    timeout: Duration,
) -> Result<Vec<u8>, ImageError> {
    let agent = agent(public_only);
    let deadline = Instant::now() + timeout;
    let mut current = web_url(url)?;
    for _ in 0..=MAX_REDIRECTS {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(ImageError::Fetch("timeout".into()));
        }
        let response = agent
            .get(current.as_str())
            .config()
            .timeout_global(Some(left))
            .build()
            .call()
            .map_err(|e| ImageError::Fetch(e.to_string()))?;
        let status = response.status();
        if status.is_redirection() {
            let location = response
                .headers()
                .get("location")
                .and_then(|v| v.to_str().ok())
                .ok_or_else(|| ImageError::Fetch(format!("{status} without a location")))?;
            let next = current
                .join(location)
                .map_err(|e| ImageError::Fetch(e.to_string()))?;
            current = web_url(next.as_str())?;
            continue;
        }
        if !status.is_success() {
            return Err(ImageError::Fetch(status.to_string()));
        }
        let mut body = response.into_body();
        if body.content_length().is_some_and(|n| n > MAX_BYTES) {
            return Err(ImageError::TooLarge);
        }
        return body
            .with_config()
            .limit(MAX_BYTES)
            .read_to_vec()
            .map_err(|e| match e {
                ureq::Error::BodyExceedsLimit(_) => ImageError::TooLarge,
                e => ImageError::Fetch(e.to_string()),
            });
    }
    Err(ImageError::Fetch("too many redirects".into()))
}
