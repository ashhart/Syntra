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
                if let Some(ref fr) = pol.file_root {
                    let root = std::path::PathBuf::from(fr);
                    if root.is_relative() {
                        if let Some(ref wd) = context.working_dir {
                            wd.join(root)
                        } else {
                            root
                        }
                    } else {
                        root
                    }
                } else if let Some(ref wd) = context.working_dir {
                    wd.clone()
                } else {
                    // Policy exists but no root configured — deny file access
                    return Err(format!(
                        "capability={effect}: no file_root or working_dir configured"
                    ));
                }
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
    let read_like =
        effect.contains("read") || effect == "file.exists" || effect == "nav.ephemerisState";
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

/// Check if a URL host is allowed by the network sandbox.
/// Returns Ok(true) if sandbox is active, Ok(false) if unrestricted.
pub(crate) fn check_network_sandbox(
    ctx: Option<&crate::context::ExecutionContext>,
    url: &str,
    cap_name: &str,
) -> Result<bool, String> {
    let context = match ctx {
        Some(c) => c,
        None => return Ok(false), // no context = unrestricted
    };
    let policy = match &context.policy {
        Some(p) => p,
        None => return Ok(false), // no policy = unrestricted
    };

    // Extract host from URL, handling IPv6 bracket syntax
    let authority = url
        .split("://")
        .nth(1)
        .unwrap_or(url)
        .split('/')
        .next()
        .unwrap_or("");
    let host = if authority.starts_with('[') {
        // IPv6: [::1] or [::1]:port
        authority
            .split(']')
            .next()
            .unwrap_or("")
            .trim_start_matches('[')
    } else {
        authority.split(':').next().unwrap_or("")
    };

    if host.is_empty() {
        return Err(format!("capability={cap_name}: cannot parse host from URL"));
    }

    // Check allowed_hosts (exact match only)
    if !policy.allowed_hosts.is_empty() {
        let allowed = policy.allowed_hosts.iter().any(|h| {
            if h.starts_with("*.") {
                // Wildcard: *.example.com matches sub.example.com and example.com
                host.ends_with(&h[1..]) || host == &h[2..]
            } else {
                host == h // exact match only — evil-example.com != example.com
            }
        });
        if !allowed {
            return Err(format!(
                "capability={cap_name}: host '{host}' not in allowed_hosts"
            ));
        }
    } else {
        return Err(format!(
            "capability={cap_name}: no allowed_hosts configured — outbound HTTP denied"
        ));
    }

    // Check private networks
    if policy.deny_private_networks {
        let lower = host.to_lowercase();
        if lower == "localhost" {
            return Err(format!(
                "capability={cap_name}: private/local host denied by policy"
            ));
        }

        // Check if host is a literal IP
        if let Ok(ip) = host.parse::<std::net::IpAddr>() {
            if is_private_ip(&ip) {
                return Err(format!(
                    "capability={cap_name}: private network denied by policy"
                ));
            }
        }

        // DNS resolution check — resolve hostname and check all IPs
        if host.parse::<std::net::IpAddr>().is_err() {
            // It's a hostname, try to resolve
            if let Ok(addrs) = std::net::ToSocketAddrs::to_socket_addrs(&(host, 80)) {
                for addr in addrs {
                    if is_private_ip(&addr.ip()) {
                        return Err(format!(
                            "capability={cap_name}: host '{host}' resolves to private IP {} — denied by policy",
                            addr.ip()
                        ));
                    }
                }
            }
        }
    }

    Ok(true) // sandbox is active
}

fn is_private_ip(ip: &std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(v4) => {
            v4.is_loopback()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || v4.octets()[0] == 10
                || (v4.octets()[0] == 172 && v4.octets()[1] >= 16 && v4.octets()[1] <= 31)
                || (v4.octets()[0] == 192 && v4.octets()[1] == 168)
                || (v4.octets()[0] == 169 && v4.octets()[1] == 254)
                || v4.is_multicast()
        }
        std::net::IpAddr::V6(v6) => {
            v6.is_loopback() || v6.is_unspecified()
            || (v6.segments()[0] & 0xfe00) == 0xfc00  // unique local
            || (v6.segments()[0] & 0xffc0) == 0xfe80  // link-local
            || v6.is_multicast()
        }
    }
}
