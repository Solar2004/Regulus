//! Outgoing requests. Every one goes out with the fixed User-Agent `regulus/<version>`
//! and no proxy (a proxy would resolve the name itself, past the guard), follows
//! redirects by hand so each hop is checked again, and reads at most `max_bytes` of body.
//!
//! The guard is the resolver: unless `allow_private`, a name that resolves to any
//! loopback, private, link-local, shared, unspecified or multicast address is refused,
//! and the connection goes to the addresses it checked (no second lookup to rebind).

use std::io::Read;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::time::Duration;

use ureq::config::Config;
use ureq::http::Uri;
use ureq::unversioned::resolver::{DefaultResolver, ResolvedSocketAddrs, Resolver};
use ureq::unversioned::transport::{DefaultConnector, NextTimeout};
use url::Url;

/// What every request says it is. No user, host or machine data.
pub const USER_AGENT: &str = concat!("regulus/", env!("CARGO_PKG_VERSION"));

/// Whether `ip` is somewhere a workflow should not reach unless the node allows it.
pub fn is_private(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => private_v4(v4),
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return private_v4(v4);
            }
            let first = v6.segments()[0];
            v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || (first & 0xfe00) == 0xfc00 // unique local fc00::/7
                || (first & 0xffc0) == 0xfe80 // link-local fe80::/10
                || (first & 0xffc0) == 0xfec0 // site-local fec0::/10 (deprecated)
                || first == 0x0064 && v6.segments()[1] == 0xff9b // NAT64 64:ff9b::/96
                || (first == 0x2002) // 6to4 embeds any v4
                || v6 == Ipv6Addr::new(0, 0, 0, 0, 0, 0, 0, 1)
        }
    }
}

fn private_v4(ip: Ipv4Addr) -> bool {
    let [a, b, ..] = ip.octets();
    ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_unspecified()
        || ip.is_broadcast()
        || ip.is_multicast()
        || a == 0
        || (a == 100 && (64..128).contains(&b)) // shared 100.64.0.0/10
        || (a == 192 && b == 0 && ip.octets()[2] == 0) // IETF 192.0.0.0/24
        || (a == 198 && (b == 18 || b == 19)) // benchmarking
        || a >= 240
}

/// The resolver that refuses private destinations.
#[derive(Debug)]
struct Guard {
    allow_private: bool,
}

impl Resolver for Guard {
    fn resolve(
        &self,
        uri: &Uri,
        config: &Config,
        timeout: NextTimeout,
    ) -> Result<ResolvedSocketAddrs, ureq::Error> {
        let found = DefaultResolver::default().resolve(uri, config, timeout)?;
        if !self.allow_private {
            if let Some(addr) = found.iter().find(|a| is_private(a.ip())) {
                let host = uri.host().unwrap_or_default();
                return Err(ureq::Error::Io(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    format!(
                        "{host} resolves to {}, a private address; the node needs \
                         `allow_private = yes` to reach it",
                        addr.ip()
                    ),
                )));
            }
        }
        Ok(found)
    }
}

/// How a node's requests go out.
#[derive(Debug, Clone)]
pub struct Rules {
    pub allow_private: bool,
    /// Every hop must be https.
    pub https_only: bool,
    pub max_redirects: u32,
    pub timeout: Duration,
    pub max_bytes: u64,
}

/// One request: what goes out on the first hop.
#[derive(Debug, Clone, Default)]
pub struct Request {
    pub method: String,
    pub url: String,
    /// Sent only to the origin of `url`: a redirect elsewhere drops them.
    pub headers: Vec<(String, String)>,
    pub body: Option<Vec<u8>>,
}

/// A response, read whole.
#[derive(Debug, Clone)]
pub struct Fetched {
    pub status: u16,
    /// Where it came from after redirects.
    pub url: String,
    pub content_type: String,
    pub body: Vec<u8>,
}

/// `url` without query or fragment, for messages: a key in a query never shows.
pub fn shown(url: &str) -> String {
    match Url::parse(url) {
        Ok(mut parsed) => {
            parsed.set_query(None);
            parsed.set_fragment(None);
            let _ = parsed.set_password(None);
            let _ = parsed.set_username("");
            parsed.to_string()
        }
        Err(_) => "<bad url>".into(),
    }
}

fn check_scheme(url: &Url, rules: &Rules) -> Result<(), String> {
    match url.scheme() {
        "https" => Ok(()),
        "http" if !rules.https_only => Ok(()),
        "http" => Err(format!(
            "{} is not https (the node needs `allow_http = yes`)",
            shown(url.as_str())
        )),
        other => Err(format!(
            "`{other}` URLs are not fetched, only http and https"
        )),
    }
}

pub fn agent(rules: &Rules) -> ureq::Agent {
    let config = ureq::Agent::config_builder()
        .timeout_global(Some(rules.timeout))
        .http_status_as_error(false)
        .max_redirects(0)
        .proxy(None)
        .user_agent(USER_AGENT)
        .build();
    ureq::Agent::with_parts(
        config,
        DefaultConnector::default(),
        Guard {
            allow_private: rules.allow_private,
        },
    )
}

/// Sends `request`, following redirects up to `max_redirects`, each hop checked, and
/// hands the final response's body to `read` with at most `max_bytes + 1` bytes
/// readable, so `read` can tell an over-long body.
pub fn send<T>(
    agent: &ureq::Agent,
    rules: &Rules,
    request: Request,
    read: impl FnOnce(u16, &str, &str, &mut dyn Read) -> Result<T, String>,
) -> Result<T, String> {
    let mut url = Url::parse(request.url.trim()).map_err(|e| format!("bad URL: {e}"))?;
    let origin = url.origin();
    let mut method = if request.method.trim().is_empty() {
        "GET".to_string()
    } else {
        request.method.trim().to_uppercase()
    };
    let mut body = request.body;
    let mut hops = 0;
    loop {
        check_scheme(&url, rules)?;
        let mut builder = ureq::http::Request::builder()
            .method(method.as_str())
            .uri(url.as_str());
        if url.origin() == origin {
            for (name, value) in &request.headers {
                builder = builder.header(name.as_str(), value.as_str());
            }
        }
        let http = builder
            .body(body.clone().unwrap_or_default())
            .map_err(|e| format!("bad request: {e}"))?;
        let mut response = agent
            .run(http)
            .map_err(|e| format!("{}: {e}", shown(url.as_str())))?;
        let status = response.status().as_u16();
        let location = response
            .headers()
            .get("location")
            .and_then(|v| v.to_str().ok())
            .map(String::from);
        if let (true, Some(location)) = (response.status().is_redirection(), location) {
            hops += 1;
            if hops > rules.max_redirects {
                return Err(format!(
                    "{} redirects more than {} times (`max_redirects`)",
                    shown(request.url.as_str()),
                    rules.max_redirects
                ));
            }
            url = url
                .join(&location)
                .map_err(|e| format!("bad redirect from {}: {e}", shown(url.as_str())))?;
            if !matches!(status, 307 | 308) {
                method = "GET".into();
                body = None;
            }
            continue;
        }
        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_string();
        let final_url = url.to_string();
        let mut reader = response.body_mut().as_reader().take(rules.max_bytes + 1);
        return read(status, &final_url, &content_type, &mut reader);
    }
}

/// [`send`], reading the body whole; past `max_bytes` it is an error.
pub fn fetch(agent: &ureq::Agent, rules: &Rules, request: Request) -> Result<Fetched, String> {
    let max = rules.max_bytes;
    send(
        agent,
        rules,
        request,
        |status, url, content_type, reader| {
            let mut body = Vec::new();
            reader
                .read_to_end(&mut body)
                .map_err(|e| format!("{}: {e}", shown(url)))?;
            if body.len() as u64 > max {
                return Err(format!(
                    "{} sent more than {max} bytes (`max_bytes`)",
                    shown(url)
                ));
            }
            Ok(Fetched {
                status,
                url: url.to_string(),
                content_type: content_type.to_string(),
                body,
            })
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_addresses_are_told_apart() {
        for ip in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "192.168.1.1",
            "169.254.169.254",
            "0.0.0.0",
            "100.64.0.1",
            "::1",
            "::",
            "fd00::1",
            "fe80::1",
            "::ffff:127.0.0.1",
            "::ffff:10.0.0.1",
            "224.0.0.1",
        ] {
            assert!(is_private(ip.parse().unwrap()), "{ip}");
        }
        for ip in ["1.1.1.1", "93.184.216.34", "2606:4700:4700::1111"] {
            assert!(!is_private(ip.parse().unwrap()), "{ip}");
        }
    }

    #[test]
    fn shown_urls_lose_their_query_and_credentials() {
        assert_eq!(
            shown("https://u:p@example.com/a?key=secret#x"),
            "https://example.com/a"
        );
    }

    #[test]
    fn the_guard_refuses_loopback_before_connecting() {
        let rules = Rules {
            allow_private: false,
            https_only: false,
            max_redirects: 0,
            timeout: Duration::from_secs(5),
            max_bytes: 10,
        };
        let request = Request {
            url: "http://127.0.0.1:9/".into(),
            ..Request::default()
        };
        let error = fetch(&agent(&rules), &rules, request).unwrap_err();
        assert!(error.contains("private address"), "{error}");
        let request = Request {
            url: "file:///etc/passwd".into(),
            ..Request::default()
        };
        assert!(fetch(&agent(&rules), &rules, request).is_err());
        let https = Rules {
            https_only: true,
            ..rules
        };
        let request = Request {
            url: "http://example.com/".into(),
            ..Request::default()
        };
        let error = fetch(&agent(&https), &https, request).unwrap_err();
        assert!(error.contains("allow_http"), "{error}");
    }
}
