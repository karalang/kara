//! Which proxy, if any, an HTTP client request goes through (docs/library/io.md
//! § HTTP client, "Proxies"). Shared by the MIR interpreter and the runtime's
//! compiled client (`runtime/src/lib.rs` includes this file by `#[path]`), so
//! both backends choose the same route and report the same errors.
//!
//! The variables are curl's and Go's: `https_proxy` / `HTTPS_PROXY` for an
//! https URL, `http_proxy` / `HTTP_PROXY` for an http one, `all_proxy` /
//! `ALL_PROXY` as the fallback, the lowercase form winning, and `no_proxy` /
//! `NO_PROXY` naming the hosts that go direct. They are read at each request.

use std::net::IpAddr;

/// The proxy a request goes through.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProxyRoute {
    /// The proxy's host, without brackets for an IPv6 address.
    pub host: String,
    pub port: u16,
    /// The proxy URL's user and password, decoded, as `user:password`.
    pub credentials: Option<String>,
}

impl ProxyRoute {
    /// The `Proxy-Authorization` header value, when the URL had credentials.
    pub fn authorization(&self) -> Option<String> {
        self.credentials
            .as_ref()
            .map(|c| format!("Basic {}", base64(c.as_bytes())))
    }

    /// `host:port`, with brackets around an IPv6 host.
    pub fn authority(&self) -> String {
        authority(&self.host, self.port)
    }
}

/// The route for a request to `host` (as a URL writes it, brackets allowed)
/// over `scheme`, read from the process environment: `Ok(None)` to connect
/// directly, or an `Err` naming the variable whose proxy cannot be used. A
/// configured proxy is never bypassed because it cannot be used.
pub fn proxy_for(scheme: &str, host: &str) -> Result<Option<ProxyRoute>, String> {
    proxy_for_env(scheme, host, |name| std::env::var(name).ok())
}

/// [`proxy_for`] over the variables `var` returns.
pub fn proxy_for_env(
    scheme: &str,
    host: &str,
    var: impl Fn(&str) -> Option<String>,
) -> Result<Option<ProxyRoute>, String> {
    let host = host
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_ascii_lowercase();
    // Localhost and loopback addresses are never proxied (Go's rule).
    if host == "localhost" || host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback()) {
        return Ok(None);
    }
    let lookup = |lower: &'static str, upper: &'static str| {
        [lower, upper].into_iter().find_map(|name| {
            var(name)
                .filter(|v| !v.trim().is_empty())
                .map(|v| (name, v))
        })
    };
    let chosen = match scheme.to_ascii_lowercase().as_str() {
        "https" => lookup("https_proxy", "HTTPS_PROXY"),
        "http" => lookup("http_proxy", "HTTP_PROXY"),
        _ => None,
    }
    .or_else(|| lookup("all_proxy", "ALL_PROXY"));
    let Some((name, value)) = chosen else {
        return Ok(None);
    };
    if let Some((_, list)) = lookup("no_proxy", "NO_PROXY") {
        if no_proxy_matches(&list, &host) {
            return Ok(None);
        }
    }
    parse_proxy(name, value.trim()).map(Some)
}

/// Whether `host` (lowercase, no brackets) is in the `no_proxy` list `list`:
/// `*`, a domain that `host` is or ends with (a leading `.` or `*.` allowed),
/// an IP address, or a CIDR block. A `:port` on an entry is ignored.
fn no_proxy_matches(list: &str, host: &str) -> bool {
    let ip = host.parse::<IpAddr>().ok();
    list.split(',').any(|entry| {
        let entry = entry.trim().to_ascii_lowercase();
        if entry.is_empty() {
            return false;
        }
        if entry == "*" {
            return true;
        }
        if let Some((net, bits)) = entry.split_once('/') {
            return match (ip, net.parse::<IpAddr>(), bits.parse::<u32>()) {
                (Some(ip), Ok(net), Ok(bits)) => in_cidr(ip, net, bits),
                _ => false,
            };
        }
        let entry = strip_port(&entry);
        if let Ok(e) = entry.parse::<IpAddr>() {
            return ip == Some(e);
        }
        let domain = entry.trim_start_matches("*.").trim_start_matches('.');
        !domain.is_empty()
            && (host == domain
                || host
                    .strip_suffix(domain)
                    .is_some_and(|rest| rest.ends_with('.')))
    })
}

/// `entry` without a trailing `:port` (an IPv6 address keeps its colons).
fn strip_port(entry: &str) -> &str {
    let entry = entry.trim_start_matches('[');
    if let Some(i) = entry.find(']') {
        return &entry[..i];
    }
    match entry.rsplit_once(':') {
        Some((h, p)) if !h.contains(':') && p.chars().all(|c| c.is_ascii_digit()) => h,
        _ => entry,
    }
}

fn in_cidr(ip: IpAddr, net: IpAddr, bits: u32) -> bool {
    match (ip, net) {
        (IpAddr::V4(a), IpAddr::V4(n)) if bits <= 32 => {
            let mask = u32::MAX.checked_shl(32 - bits).unwrap_or(0);
            u32::from(a) & mask == u32::from(n) & mask
        }
        (IpAddr::V6(a), IpAddr::V6(n)) if bits <= 128 => {
            let mask = u128::MAX.checked_shl(128 - bits).unwrap_or(0);
            u128::from(a) & mask == u128::from(n) & mask
        }
        _ => false,
    }
}

/// The proxy `value` of the variable `name` names: `http://[user[:password]@]host[:port][/]`
/// or a bare `host:port`, read as http, the port 80 when absent. The error
/// names the variable but never repeats its value, which can hold a password.
fn parse_proxy(name: &str, value: &str) -> Result<ProxyRoute, String> {
    let rest = match value.split_once("://") {
        Some((scheme, rest)) if scheme.eq_ignore_ascii_case("http") => rest,
        Some((scheme, _)) => {
            return Err(format!(
                "{name} names a `{scheme}://` proxy, and only an `http://` proxy is supported"
            ))
        }
        None => value,
    };
    let bad = || format!("{name} is not a proxy URL (`http://host:port`)");
    let rest = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let (userinfo, hostport) = match rest.rsplit_once('@') {
        Some((u, h)) => (Some(u), h),
        None => (None, rest),
    };
    let (host, port) = if let Some(after) = hostport.strip_prefix('[') {
        let (h, tail) = after.split_once(']').ok_or_else(bad)?;
        match tail {
            "" => (h, None),
            t => (h, Some(t.strip_prefix(':').ok_or_else(bad)?)),
        }
    } else {
        match hostport.rsplit_once(':') {
            Some((h, p)) => (h, Some(p)),
            None => (hostport, None),
        }
    };
    if host.is_empty() || host.contains(['@', ' ']) {
        return Err(bad());
    }
    let port = match port {
        Some(p) => p.parse::<u16>().map_err(|_| bad())?,
        None => 80,
    };
    let credentials = userinfo.map(|u| match u.split_once(':') {
        Some((user, password)) => format!("{}:{}", percent_decode(user), percent_decode(password)),
        None => format!("{}:", percent_decode(u)),
    });
    Ok(ProxyRoute {
        host: host.to_string(),
        port,
        credentials,
    })
}

/// The scheme and host of `url` (`scheme://[userinfo@]host[:port]/...`), or
/// `None` when it has no scheme.
pub fn url_scheme_host(url: &str) -> Option<(String, String)> {
    let (scheme, rest) = url.split_once("://")?;
    let auth = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let hostport = auth.rsplit_once('@').map_or(auth, |(_, h)| h);
    let host = if let Some(after) = hostport.strip_prefix('[') {
        after.split(']').next().unwrap_or_default()
    } else {
        hostport.rsplit_once(':').map_or(hostport, |(h, _)| h)
    };
    Some((scheme.to_ascii_lowercase(), host.to_string()))
}

/// The port of `url`, or `scheme`'s default (80 for http, 443 for https).
pub fn url_port(url: &str, scheme: &str) -> Option<u16> {
    let (_, rest) = url.split_once("://")?;
    let auth = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let hostport = auth.rsplit_once('@').map_or(auth, |(_, h)| h);
    let port = match hostport.rsplit_once(']') {
        Some((_, tail)) => tail.strip_prefix(':'),
        None => hostport.rsplit_once(':').map(|(_, p)| p),
    };
    match port {
        Some(p) => p.parse().ok(),
        None if scheme == "https" => Some(443),
        None => Some(80),
    }
}

/// The origin of `url`: its scheme and host, lower-cased, and its port, the
/// scheme's default when it names none.
pub fn url_origin(url: &str) -> Option<(String, String, u16)> {
    let (scheme, host) = url_scheme_host(url)?;
    let port = url_port(url, &scheme)?;
    Some((scheme, host.to_ascii_lowercase(), port))
}

/// Whether a redirect from the request for `first` to `next` keeps the
/// caller's credentials: only while it stays on `first`'s origin (the Fetch
/// standard's rule, which curl follows too).
pub fn redirect_keeps_credentials(first: &str, next: &str) -> bool {
    let origin = url_origin(first);
    origin.is_some() && origin == url_origin(next)
}

/// Whether `name` is a header a redirect off the origin drops.
pub fn is_credential_header(name: &str) -> bool {
    name.eq_ignore_ascii_case("authorization") || name.eq_ignore_ascii_case("cookie")
}

/// The CONNECT request that opens a tunnel to `host:port` through `route`.
pub fn connect_request(route: &ProxyRoute, host: &str, port: u16) -> String {
    let target = authority(host.trim_start_matches('[').trim_end_matches(']'), port);
    let auth = route
        .authorization()
        .map(|a| format!("Proxy-Authorization: {a}\r\n"))
        .unwrap_or_default();
    format!("CONNECT {target} HTTP/1.1\r\nHost: {target}\r\n{auth}\r\n")
}

/// Why the proxy's answer `head` (its bytes up to the blank line) to a
/// CONNECT does not open the tunnel, or `None` when it is a 2xx.
pub fn connect_refusal(head: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(head);
    let line = text.lines().next().unwrap_or_default().trim();
    let mut parts = line.splitn(3, ' ');
    let version = parts.next().unwrap_or_default();
    let code = parts.next().and_then(|c| c.parse::<u16>().ok());
    match code {
        Some(c) if version.starts_with("HTTP/") && (200..300).contains(&c) => None,
        Some(c) => {
            let reason = parts.next().unwrap_or_default().trim();
            Some(
                format!("the proxy refused the tunnel: {c} {reason}")
                    .trim_end()
                    .to_string(),
            )
        }
        None => Some("the proxy's answer to CONNECT is not HTTP".to_string()),
    }
}

/// The message for a CONNECT the proxy at `route` could not be reached for.
pub fn proxy_unreachable(route: &ProxyRoute, cause: &str) -> String {
    format!("cannot reach the proxy {}: {cause}", route.authority())
}

fn authority(host: &str, port: u16) -> String {
    if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            if let Some(Ok(v)) = s.get(i + 1..i + 3).map(|h| u8::from_str_radix(h, 16)) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn base64(input: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |acc, (i, &b)| acc | (u32::from(b) << (16 - 8 * i)));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(T[(n >> (18 - 6 * i)) as usize & 63] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn route(
        scheme: &str,
        host: &str,
        vars: &[(&str, &str)],
    ) -> Result<Option<ProxyRoute>, String> {
        proxy_for_env(scheme, host, |n| {
            vars.iter()
                .find(|(k, _)| *k == n)
                .map(|(_, v)| v.to_string())
        })
    }

    #[test]
    fn the_variable_follows_the_scheme_and_lowercase_wins() {
        let vars = [
            ("HTTPS_PROXY", "http://s:1"),
            ("http_proxy", "h:2"),
            ("HTTP_PROXY", "H:3"),
        ];
        assert_eq!(route("https", "a.com", &vars).unwrap().unwrap().port, 1);
        assert_eq!(route("http", "a.com", &vars).unwrap().unwrap().host, "h");
        let all = [("ALL_PROXY", "http://all:9"), ("https_proxy", "")];
        assert_eq!(route("https", "a.com", &all).unwrap().unwrap().port, 9);
        assert_eq!(
            route("http", "a.com", &[("HTTPS_PROXY", "s:1")]).unwrap(),
            None
        );
    }

    #[test]
    fn no_proxy_and_loopback_go_direct() {
        let p = ("HTTPS_PROXY", "http://p:8080");
        for (host, list, direct) in [
            ("api.example.com", "example.com", true),
            ("api.example.com", ".example.com", true),
            ("example.com", "*.example.com", true),
            ("notexample.com", "example.com", false),
            ("10.1.2.3", "10.0.0.0/8", true),
            ("11.1.2.3", "10.0.0.0/8", false),
            ("192.168.1.5", "192.168.1.5", true),
            ("[2001:db8::1]", "2001:db8::/32", true),
            ("x.org", "*", true),
            ("x.org", "a.com:443, x.org:80", true),
        ] {
            let got = route("https", host, &[p, ("no_proxy", list)]).unwrap();
            assert_eq!(got.is_none(), direct, "{host} against {list}");
        }
        for host in ["localhost", "127.0.0.1", "127.9.9.9", "[::1]"] {
            assert_eq!(route("https", host, &[p]).unwrap(), None, "{host}");
        }
    }

    #[test]
    fn proxy_urls_parse_or_name_the_variable() {
        let r = route(
            "https",
            "a.com",
            &[("HTTPS_PROXY", "http://us%40r:p%3Ass@[::1]:3128/")],
        )
        .unwrap()
        .unwrap();
        assert_eq!((r.host.as_str(), r.port), ("::1", 3128));
        assert_eq!(r.credentials.as_deref(), Some("us@r:p:ss"));
        assert_eq!(
            r.authorization().unwrap(),
            format!("Basic {}", base64(b"us@r:p:ss"))
        );
        let r = route("https", "a.com", &[("HTTPS_PROXY", "proxy.local")])
            .unwrap()
            .unwrap();
        assert_eq!(r.port, 80);
        let e = route(
            "https",
            "a.com",
            &[("HTTPS_PROXY", "socks5://u:secret@p:1")],
        )
        .unwrap_err();
        assert!(
            e.starts_with("HTTPS_PROXY names a `socks5://` proxy"),
            "{e}"
        );
        assert!(!e.contains("secret"), "{e}");
        let e = route("https", "a.com", &[("all_proxy", "http://p:notaport")]).unwrap_err();
        assert!(e.starts_with("all_proxy is not a proxy URL"), "{e}");
    }

    #[test]
    fn credentials_stay_on_the_origin() {
        let first = "https://Api.example.com/v1";
        assert!(redirect_keeps_credentials(
            first,
            "https://api.example.com:443/v1/"
        ));
        assert!(!redirect_keeps_credentials(
            first,
            "http://api.example.com/v1"
        ));
        assert!(!redirect_keeps_credentials(
            first,
            "https://api.example.com:8443/v1"
        ));
        assert!(!redirect_keeps_credentials(
            first,
            "https://cdn.example.com/v1"
        ));
        assert!(is_credential_header("Cookie") && is_credential_header("AUTHORIZATION"));
        assert!(!is_credential_header("Accept"));
    }

    #[test]
    fn connect_requests_and_answers() {
        let r = ProxyRoute {
            host: "p".into(),
            port: 8,
            credentials: Some("a:b".into()),
        };
        assert_eq!(
            connect_request(&r, "[::1]", 443),
            "CONNECT [::1]:443 HTTP/1.1\r\nHost: [::1]:443\r\nProxy-Authorization: Basic YTpi\r\n\r\n"
        );
        assert_eq!(
            connect_refusal(b"HTTP/1.1 200 Connection established\r\n"),
            None
        );
        assert_eq!(
            connect_refusal(b"HTTP/1.1 403 Forbidden\r\n").unwrap(),
            "the proxy refused the tunnel: 403 Forbidden"
        );
        assert_eq!(base64(b"hello"), "aGVsbG8=");
        assert_eq!(
            url_scheme_host("HTTPS://u@[::1]:8/x").unwrap(),
            ("https".into(), "::1".into())
        );
    }
}
