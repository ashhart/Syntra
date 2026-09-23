//! Filesystem and network sandbox enforcement.

/// Resolve a file path inside the sandbox root. Returns the canonical path or error.
pub(crate) fn resolve_sandbox_path(
    ctx: Option<&crate::context::ExecutionContext>,
    requested: &str,
    effect: &str,
) -> Result<std::path::PathBuf, String> {
    // Determine root — no context or no policy = unrestricted
    let root = match ctx {
        Some(context) => match &context.policy {
            Some(pol) => {
                // The root always lives inside the execution's working
                // directory. `file_root` can only narrow it: it is re-validated
                // here (relative, no `..`) even though policy parsing already
                // enforces that, because policies can also be built in code.
                let wd = context.working_dir.as_ref().ok_or_else(|| {
                    format!("capability={effect}: no working_dir configured for the file sandbox")
                })?;
                let root = match pol.file_root.as_deref() {
                    Some(fr) => {
                        crate::context::validate_file_root(fr)
                            .map_err(|e| format!("capability={effect}: {e}"))?;
                        wd.join(fr)
                    }
                    None => wd.clone(),
                };
                // A symlinked component in file_root must not lift the root
                // out of the working directory.
                if let (Ok(canon_root), Ok(canon_wd)) = (root.canonicalize(), wd.canonicalize()) {
                    if !canon_root.starts_with(&canon_wd) {
                        return Err(format!(
                            "capability={effect}: file_root escapes the working directory"
                        ));
                    }
                }
                root
            }
            None => return Ok(std::path::PathBuf::from(requested)), // no policy = unrestricted
        },
        None => return Ok(std::path::PathBuf::from(requested)), // no context = unrestricted
    };

    // Sandbox active: reject absolute paths and traversal
    if requested.starts_with('/') || requested.starts_with('\\') {
        return Err(format!(
            "capability={effect}: absolute paths denied by sandbox"
        ));
    }
    if requested.contains("..") {
        return Err(format!(
            "capability={effect}: path traversal denied by sandbox"
        ));
    }

    let target = root.join(requested);

    // For reads: canonicalize and verify inside root
    let read_like = effect.contains("read") || effect == "file.exists";
    if read_like {
        if target.exists() {
            let canon = target
                .canonicalize()
                .map_err(|e| format!("capability={effect}: cannot resolve path: {e}"))?;
            let canon_root = root
                .canonicalize()
                .map_err(|e| format!("capability={effect}: cannot resolve root: {e}"))?;
            if !canon.starts_with(&canon_root) {
                return Err(format!("capability={effect}: path escapes sandbox"));
            }
            Ok(canon)
        } else {
            // File doesn't exist — for file.exists that's fine, return the joined path
            Ok(target)
        }
    } else {
        // For writes: must defeat symlink-escape attacks.
        //
        // Two cases:
        //   (a) target file already exists — it might be a symlink whose
        //       resolved target points outside the sandbox root. Canonicalize
        //       the *target itself* (which resolves any symlinks) and assert
        //       the canonical path is inside the canonical root. Otherwise a
        //       writer would clobber whatever the symlink points to.
        //   (b) target file does not yet exist — the filename component
        //       cannot itself be a symlink because the file doesn't exist,
        //       but the parent dir might be a symlink. Canonicalize the
        //       parent and re-form the write path as
        //       `canonical_parent.join(filename)` so the eventual write
        //       lands in the real directory. The parent must exist and be
        //       inside the canonical root.
        let canon_root = root
            .canonicalize()
            .map_err(|e| format!("capability={effect}: cannot resolve root: {e}"))?;
        if target.exists() {
            // Existing file/symlink — canonicalize the target itself,
            // which follows symlinks. If it lands outside the sandbox the
            // write would escape, so refuse.
            let canon_target = target
                .canonicalize()
                .map_err(|e| format!("capability={effect}: cannot resolve path: {e}"))?;
            if !canon_target.starts_with(&canon_root) {
                return Err(format!("capability={effect}: path escapes sandbox"));
            }
            Ok(canon_target)
        } else {
            // New file — parent must exist (otherwise we can't write).
            let parent = target
                .parent()
                .ok_or_else(|| format!("capability={effect}: target has no parent directory"))?;
            if !parent.exists() {
                return Err(format!(
                    "capability={effect}: parent directory does not exist"
                ));
            }
            let canon_parent = parent
                .canonicalize()
                .map_err(|e| format!("capability={effect}: cannot resolve parent: {e}"))?;
            if !canon_parent.starts_with(&canon_root) {
                return Err(format!("capability={effect}: path escapes sandbox"));
            }
            let filename = target
                .file_name()
                .ok_or_else(|| format!("capability={effect}: target has no filename"))?;
            Ok(canon_parent.join(filename))
        }
    }
}

/// Network permissions for one sandboxed request. The allow-list and
/// private-address checks run inside the HTTP client's DNS resolver, on
/// the exact host the client is about to connect to, and the addresses
/// that pass are the ones it connects to. That closes two holes a
/// pre-flight check leaves open: a URL parser that disagrees with the
/// client about which host a URL names, and DNS answers that change
/// between the check and the connect (rebinding).
#[derive(Debug, Clone)]
pub(crate) struct NetworkGuard {
    allowed_hosts: Vec<String>,
    deny_private: bool,
    capability: String,
}

impl NetworkGuard {
    /// Build a `ureq` agent that enforces this guard: no redirects and
    /// every connection resolved through [`NetworkGuard::resolve`].
    pub(crate) fn agent(&self) -> ureq::Agent {
        let guard = self.clone();
        ureq::AgentBuilder::new()
            .redirects(0)
            .resolver(move |netloc: &str| guard.resolve(netloc))
            .build()
    }

    fn resolve(&self, netloc: &str) -> std::io::Result<Vec<std::net::SocketAddr>> {
        let denied = |msg: String| std::io::Error::new(std::io::ErrorKind::PermissionDenied, msg);
        let cap = &self.capability;
        let (host, _port) = netloc.rsplit_once(':').ok_or_else(|| {
            denied(format!(
                "capability={cap}: cannot parse host from '{netloc}'"
            ))
        })?;
        let host = host.trim_start_matches('[').trim_end_matches(']');
        let host = host.trim_end_matches('.').to_ascii_lowercase();
        if !host_allowed(&host, &self.allowed_hosts) {
            return Err(denied(format!(
                "capability={cap}: host '{host}' not in allowed_hosts"
            )));
        }
        let addrs: Vec<std::net::SocketAddr> =
            std::net::ToSocketAddrs::to_socket_addrs(netloc)?.collect();
        if addrs.is_empty() {
            return Err(denied(format!(
                "capability={cap}: host '{host}' did not resolve"
            )));
        }
        if self.deny_private {
            if let Some(bad) = addrs.iter().find(|a| is_private_ip(&a.ip())) {
                return Err(denied(format!(
                    "capability={cap}: host '{host}' resolves to private IP {} — denied by policy",
                    bad.ip()
                )));
            }
        }
        Ok(addrs)
    }
}

/// Exact match, or a `*.suffix` entry matching the suffix itself and any
/// subdomain of it (on a label boundary, so `evil-example.com` never
/// matches `*.example.com`).
fn host_allowed(host: &str, allowed: &[String]) -> bool {
    allowed.iter().any(|h| match h.strip_prefix("*.") {
        Some(suffix) => {
            host == suffix
                || (host.len() > suffix.len()
                    && host.ends_with(suffix)
                    && host.as_bytes()[host.len() - suffix.len() - 1] == b'.')
        }
        None => host == h,
    })
}

/// Host portion of a URL for early, human-readable denials. The resolver
/// check in [`NetworkGuard`] is the authoritative one; this only has to be
/// conservative (anything unusual is refused).
fn url_host(url: &str) -> Option<String> {
    let rest = url.split_once("://")?.1;
    let end = rest
        .find(|c: char| c == '/' || c == '?' || c == '#' || c == '\\')
        .unwrap_or(rest.len());
    let authority = &rest[..end];
    if authority.contains('@') {
        return None; // userinfo is never needed by a capsule and hides the real host
    }
    let host = if let Some(stripped) = authority.strip_prefix('[') {
        stripped.split(']').next()?
    } else {
        authority.split(':').next()?
    };
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    (!host.is_empty()).then_some(host)
}

/// Check a URL against the network sandbox. Returns `Ok(None)` when no
/// sandbox applies (no context or no policy), or the guard the request
/// must be sent through.
pub(crate) fn check_network_sandbox(
    ctx: Option<&crate::context::ExecutionContext>,
    url: &str,
    cap_name: &str,
) -> Result<Option<NetworkGuard>, String> {
    let context = match ctx {
        Some(c) => c,
        None => return Ok(None), // no context = unrestricted
    };
    let policy = match &context.policy {
        Some(p) => p,
        None => return Ok(None), // no policy = unrestricted
    };

    let scheme = url
        .split_once("://")
        .map(|(s, _)| s.to_ascii_lowercase())
        .unwrap_or_default();
    match scheme.as_str() {
        "https" => {}
        "http" if policy.allow_insecure_http => {}
        "http" => {
            return Err(format!(
                "capability={cap_name}: plain http:// is denied by policy (use https:// or set allow_insecure_http)"
            ));
        }
        _ => return Err(format!("capability={cap_name}: unsupported URL scheme")),
    }

    let host = url_host(url)
        .ok_or_else(|| format!("capability={cap_name}: cannot parse host from URL"))?;

    if policy.allowed_hosts.is_empty() {
        return Err(format!(
            "capability={cap_name}: no allowed_hosts configured — outbound HTTP denied"
        ));
    }
    if !host_allowed(&host, &policy.allowed_hosts) {
        return Err(format!(
            "capability={cap_name}: host '{host}' not in allowed_hosts"
        ));
    }

    if policy.deny_private_networks {
        if host == "localhost" || host.ends_with(".localhost") {
            return Err(format!(
                "capability={cap_name}: private/local host denied by policy"
            ));
        }
        if let Ok(ip) = host.parse::<std::net::IpAddr>() {
            if is_private_ip(&ip) {
                return Err(format!(
                    "capability={cap_name}: private network denied by policy"
                ));
            }
        }
    }

    Ok(Some(NetworkGuard {
        allowed_hosts: policy.allowed_hosts.clone(),
        deny_private: policy.deny_private_networks,
        capability: cap_name.to_string(),
    }))
}

/// True for every address a sandboxed capsule must not reach: loopback,
/// private, shared (CGNAT), link-local, multicast, reserved, documentation
/// and benchmarking ranges, plus IPv6 forms that embed or tunnel to an
/// IPv4 address (mapped, compatible, NAT64, 6to4, Teredo).
pub(crate) fn is_private_ip(ip: &std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(v4) => is_private_v4(v4),
        std::net::IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_private_v4(&v4);
            }
            let seg = v6.segments();
            // IPv4-compatible (::a.b.c.d, deprecated) — but not :: or ::1
            if seg[..6].iter().all(|&s| s == 0) && !v6.is_unspecified() && !v6.is_loopback() {
                let o = v6.octets();
                return is_private_v4(&std::net::Ipv4Addr::new(o[12], o[13], o[14], o[15]));
            }
            // NAT64 well-known prefix 64:ff9b::/96 embeds an IPv4 address
            if seg[0] == 0x64 && seg[1] == 0xff9b && seg[2..6].iter().all(|&s| s == 0) {
                let o = v6.octets();
                return is_private_v4(&std::net::Ipv4Addr::new(o[12], o[13], o[14], o[15]));
            }
            // 6to4 2002::/16 embeds an IPv4 address in bits 16..48
            if seg[0] == 0x2002 {
                let o = v6.octets();
                return is_private_v4(&std::net::Ipv4Addr::new(o[2], o[3], o[4], o[5]));
            }
            v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || (seg[0] & 0xfe00) == 0xfc00 // unique local fc00::/7
                || (seg[0] & 0xffc0) == 0xfe80 // link-local fe80::/10
                || (seg[0] & 0xffc0) == 0xfec0 // site-local fec0::/10 (deprecated)
                || (seg[0] == 0x64 && seg[1] == 0xff9b && seg[2] == 1) // local-use NAT64 64:ff9b:1::/48
                || (seg[0] == 0x0100 && seg[1..4].iter().all(|&s| s == 0)) // discard-only 100::/64
                || (seg[0] == 0x2001 && seg[1] == 0) // Teredo 2001::/32 tunnels to IPv4
                || (seg[0] == 0x2001 && seg[1] == 0x0db8) // documentation 2001:db8::/32
        }
    }
}

fn is_private_v4(v4: &std::net::Ipv4Addr) -> bool {
    let o = v4.octets();
    o[0] == 0 // "this network" 0.0.0.0/8
        || o[0] == 10 // 10.0.0.0/8
        || o[0] == 127 // loopback
        || (o[0] == 100 && (o[1] & 0xc0) == 64) // shared address space (CGNAT) 100.64.0.0/10
        || (o[0] == 169 && o[1] == 254) // link-local, cloud metadata
        || (o[0] == 172 && (o[1] & 0xf0) == 16) // 172.16.0.0/12
        || (o[0] == 192 && o[1] == 0 && o[2] == 0) // IETF protocol assignments 192.0.0.0/24
        || (o[0] == 192 && o[1] == 0 && o[2] == 2) // TEST-NET-1
        || (o[0] == 192 && o[1] == 88 && o[2] == 99) // 6to4 relay anycast
        || (o[0] == 192 && o[1] == 168) // 192.168.0.0/16
        || (o[0] == 198 && (o[1] & 0xfe) == 18) // benchmarking 198.18.0.0/15
        || (o[0] == 198 && o[1] == 51 && o[2] == 100) // TEST-NET-2
        || (o[0] == 203 && o[1] == 0 && o[2] == 113) // TEST-NET-3
        || o[0] >= 224 // multicast 224/4, reserved 240/4, broadcast
}

#[cfg(test)]
mod network_tests {
    use super::*;
    use std::net::IpAddr;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn private_ranges_are_denied() {
        for s in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "172.31.255.255",
            "192.168.1.1",
            "169.254.169.254",
            "100.64.0.1",
            "100.127.255.254",
            "0.0.0.0",
            "0.1.2.3",
            "192.0.0.8",
            "198.18.0.1",
            "224.0.0.1",
            "240.0.0.1",
            "255.255.255.255",
            "::1",
            "::",
            "fc00::1",
            "fd12::1",
            "fe80::1",
            "fec0::1",
            "ff02::1",
            "::ffff:127.0.0.1",
            "::ffff:169.254.169.254",
            "::ffff:10.0.0.1",
            "::127.0.0.1",
            "64:ff9b::a9fe:a9fe",
            "64:ff9b:1::1",
            "2002:0a00:0001::1",
            "2002:7f00:0001::1",
            "2001::1",
            "2001:db8::1",
            "100::1",
        ] {
            assert!(is_private_ip(&ip(s)), "{s} must be treated as private");
        }
    }

    #[test]
    fn public_addresses_are_allowed() {
        for s in [
            "8.8.8.8",
            "1.1.1.1",
            "172.32.0.1",
            "100.128.0.1",
            "192.169.0.1",
            "2606:4700:4700::1111",
            "::ffff:8.8.8.8",
            "2002:0808:0808::1",
            "64:ff9b::808:808",
        ] {
            assert!(!is_private_ip(&ip(s)), "{s} must be treated as public");
        }
    }

    #[test]
    fn wildcard_matches_only_on_label_boundaries() {
        let allowed = vec!["*.example.com".to_string(), "api.test.io".to_string()];
        assert!(host_allowed("example.com", &allowed));
        assert!(host_allowed("a.b.example.com", &allowed));
        assert!(host_allowed("api.test.io", &allowed));
        assert!(!host_allowed("evil-example.com", &allowed));
        assert!(!host_allowed("example.com.evil.io", &allowed));
        assert!(!host_allowed("x.api.test.io", &allowed));
    }

    #[test]
    fn url_host_is_conservative() {
        assert_eq!(
            url_host("https://api.example.com/x").as_deref(),
            Some("api.example.com")
        );
        assert_eq!(
            url_host("https://evil.com?.example.com").as_deref(),
            Some("evil.com")
        );
        assert_eq!(
            url_host("https://evil.com#.example.com").as_deref(),
            Some("evil.com")
        );
        assert_eq!(
            url_host("https://evil.com\\.example.com/").as_deref(),
            Some("evil.com")
        );
        assert_eq!(url_host("https://[::1]:8443/").as_deref(), Some("::1"));
        assert_eq!(
            url_host("https://example.com./").as_deref(),
            Some("example.com")
        );
        assert_eq!(url_host("https://example.com@evil.com/"), None);
    }

    fn sandboxed(allowed: &[&str], insecure: bool) -> crate::context::ExecutionContext {
        let mut policy = crate::context::ExecutionPolicy::deny_all();
        policy.allow_network = true;
        policy.allow_insecure_http = insecure;
        policy.allowed_hosts = allowed.iter().map(|s| s.to_string()).collect();
        crate::context::ExecutionContext::with_policy(policy)
    }

    #[test]
    fn plain_http_needs_explicit_opt_in() {
        let ctx = sandboxed(&["api.example.com"], false);
        let err =
            check_network_sandbox(Some(&ctx), "http://api.example.com/", "http.get").unwrap_err();
        assert!(err.contains("plain http:// is denied"), "{err}");
        let ctx = sandboxed(&["api.example.com"], true);
        assert!(
            check_network_sandbox(Some(&ctx), "http://api.example.com/", "http.get")
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn parser_confusion_urls_are_denied_before_any_request() {
        let ctx = sandboxed(&["*.example.com"], false);
        for url in [
            "https://evil.com?.example.com",
            "https://evil.com#.example.com",
            "https://evil.com\\.example.com/",
            "https://example.com@evil.com/",
            "https://[::ffff:169.254.169.254]/",
            "https://localhost/",
        ] {
            assert!(
                check_network_sandbox(Some(&ctx), url, "http.get").is_err(),
                "{url} must be denied"
            );
        }
    }

    #[test]
    fn resolver_rechecks_the_connected_host() {
        let guard = NetworkGuard {
            allowed_hosts: vec!["*.example.com".to_string()],
            deny_private: true,
            capability: "http.get".to_string(),
        };
        let err = guard.resolve("evil.com:443").unwrap_err();
        assert!(err.to_string().contains("not in allowed_hosts"), "{err}");
        // IP literals the URL layer did not catch are still refused here.
        let guard = NetworkGuard {
            allowed_hosts: vec!["127.0.0.1".to_string(), "::1".to_string()],
            deny_private: true,
            capability: "http.get".to_string(),
        };
        assert!(
            guard
                .resolve("127.0.0.1:443")
                .unwrap_err()
                .to_string()
                .contains("private IP")
        );
        assert!(
            guard
                .resolve("[::1]:443")
                .unwrap_err()
                .to_string()
                .contains("private IP")
        );
    }
}
