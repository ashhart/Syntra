//! Scoped token store for Syntra HTTP auth.
//! Scopes: `Admin`, `TenantAdmin { tenant }`, `Read { tenant, job, capsule }`.
//! Tokens are SHA-256 hashed in `tokens.json` at the store root.
//! `--admin-key` is accepted as an in-memory `Admin` token (not persisted).

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Scope {
    Admin,
    TenantAdmin { tenant: String },
    Read { tenant: String, job: String, capsule: String },
}

impl Scope {
    pub fn is_admin(&self) -> bool { matches!(self, Scope::Admin) }

    /// Decide whether this scope authorizes the requested action.
    pub fn allows(&self, action: &Action) -> bool {
        match (self, action) {
            (Scope::Admin, _) => true,

            (Scope::TenantAdmin { tenant }, Action::TenantOp { tenant: t })
            | (Scope::TenantAdmin { tenant }, Action::CapsuleRead { tenant: t, .. })
            | (Scope::TenantAdmin { tenant }, Action::CapsuleDecide { tenant: t, .. })
            | (Scope::TenantAdmin { tenant }, Action::CapsuleMutate { tenant: t, .. }) => {
                tenant == t
            }

            (Scope::Read { tenant, job, capsule },
             Action::CapsuleDecide { tenant: t, job: j, capsule: c })
            | (Scope::Read { tenant, job, capsule },
               Action::CapsuleRead { tenant: t, job: j, capsule: c }) => {
                tenant == t && job == j && capsule == c
            }

            _ => false,
        }
    }
}

/// What the caller is trying to do; derived from the route path and fed to
/// `Scope::allows`.
#[derive(Debug, Clone)]
pub enum Action<'a> {
    AdminGlobal,
    TenantOp { tenant: &'a str },
    CapsuleRead { tenant: &'a str, job: &'a str, capsule: &'a str },
    CapsuleDecide { tenant: &'a str, job: &'a str, capsule: &'a str },
    CapsuleMutate { tenant: &'a str, job: &'a str, capsule: &'a str },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenRecord {
    pub scope: Scope,
    pub created_at: u64,
    pub expires_at: Option<u64>,
    pub label: String,
}

impl TokenRecord {
    pub fn is_expired(&self, now: u64) -> bool {
        self.expires_at.map(|t| t <= now).unwrap_or(false)
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct OnDisk {
    v: u32,
    tokens: HashMap<String, TokenRecord>,
}

pub struct TokenStore {
    path: PathBuf,
    /// hash → record; reloaded on each mutation.
    tokens: HashMap<String, TokenRecord>,
}

impl TokenStore {
    pub fn load_or_init(store_root: &Path) -> Self {
        let path = store_root.join("tokens.json");
        let tokens: HashMap<String, TokenRecord> = std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str::<OnDisk>(&s).ok())
            .map(|d| d.tokens)
            .unwrap_or_default();
        Self { path, tokens }
    }

    fn flush(&self) -> Result<(), String> {
        let disk = OnDisk { v: 1, tokens: self.tokens.clone() };
        let text = serde_json::to_string_pretty(&disk)
            .map_err(|e| format!("serialize tokens: {e}"))?;
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("mkdir token-store: {e}"))?;
        }
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, text)
            .map_err(|e| format!("write tokens.tmp: {e}"))?;
        std::fs::rename(&tmp, &self.path)
            .map_err(|e| format!("rename tokens.tmp: {e}"))
    }

    /// Look up a token by its raw value. Returns the record if found AND
    /// not expired; otherwise None. Constant-time-ish: the HashMap lookup
    /// itself is not perfectly constant-time, but the value comparison the
    /// caller does (against the well-known scope structure) is — and
    /// crucially we never compare the raw token bytes against another raw
    /// token, only against pre-hashed entries.
    pub fn lookup(&self, raw_token: &str, now: u64) -> Option<&TokenRecord> {
        let hash = sha256_hex(raw_token.as_bytes());
        let rec = self.tokens.get(&hash)?;
        if rec.is_expired(now) { return None; }
        Some(rec)
    }

    /// Issue a new token. Returns `(raw_token, hash)`. The raw value is
    /// only ever returned here — after this point, only the hash is stored.
    pub fn issue(&mut self, scope: Scope, ttl_seconds: Option<u64>, label: String,
                 now: u64) -> Result<(String, String), String> {
        let raw = generate_token();
        let hash = sha256_hex(raw.as_bytes());
        let expires_at = ttl_seconds.map(|t| now + t);
        let rec = TokenRecord { scope, created_at: now, expires_at, label };
        self.tokens.insert(hash.clone(), rec);
        self.flush()?;
        Ok((raw, hash))
    }

    pub fn revoke(&mut self, hash: &str) -> Result<bool, String> {
        let removed = self.tokens.remove(hash).is_some();
        if removed { self.flush()?; }
        Ok(removed)
    }

    pub fn list(&self) -> Vec<(String, TokenRecord)> {
        self.tokens.iter().map(|(h, r)| (h.clone(), r.clone())).collect()
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    // Defer to the same sha256 used elsewhere in the crate.
    crate::store::sha256_hex(bytes)
}

/// Generate a token: 32 bytes of OS-backed cryptographic entropy hex-encoded
/// → 64-char lower-case hex string. Panics if the OS CSPRNG is unavailable —
/// in that state the box cannot safely issue bearer tokens at all.
fn generate_token() -> String {
    let mut buf = [0u8; 32];
    getrandom::getrandom(&mut buf).expect("OsRng must be available");
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap().as_secs()
    }

    struct TempDir(PathBuf);
    impl TempDir {
        fn new() -> std::io::Result<Self> {
            use std::sync::atomic::{AtomicU64, Ordering};
            static SEQ: AtomicU64 = AtomicU64::new(0);
            let seq = SEQ.fetch_add(1, Ordering::Relaxed);
            let p = std::env::temp_dir().join(format!(
                "syntra-tokens-test-{}-{:?}-{}-{}",
                std::process::id(),
                std::thread::current().id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap().as_nanos(),
                seq,
            ));
            std::fs::create_dir_all(&p)?;
            Ok(TempDir(p))
        }
        fn path(&self) -> &Path { &self.0 }
    }
    impl Drop for TempDir {
        fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); }
    }

    #[test]
    fn scope_admin_allows_everything() {
        let s = Scope::Admin;
        assert!(s.allows(&Action::AdminGlobal));
        assert!(s.allows(&Action::TenantOp { tenant: "x" }));
        assert!(s.allows(&Action::CapsuleDecide { tenant: "x", job: "j", capsule: "c" }));
        assert!(s.allows(&Action::CapsuleMutate { tenant: "x", job: "j", capsule: "c" }));
    }

    #[test]
    fn scope_tenant_admin_is_tenant_scoped() {
        let s = Scope::TenantAdmin { tenant: "acme".into() };
        assert!(s.allows(&Action::TenantOp { tenant: "acme" }));
        assert!(s.allows(&Action::CapsuleMutate { tenant: "acme", job: "j", capsule: "c" }));
        assert!(!s.allows(&Action::TenantOp { tenant: "other" }));
        assert!(!s.allows(&Action::AdminGlobal));
    }

    #[test]
    fn scope_read_only_allows_decide_and_read() {
        let s = Scope::Read { tenant: "t".into(), job: "j".into(), capsule: "c".into() };
        assert!(s.allows(&Action::CapsuleDecide { tenant: "t", job: "j", capsule: "c" }));
        assert!(s.allows(&Action::CapsuleRead   { tenant: "t", job: "j", capsule: "c" }));
        assert!(!s.allows(&Action::CapsuleMutate { tenant: "t", job: "j", capsule: "c" }));
        assert!(!s.allows(&Action::CapsuleDecide { tenant: "t", job: "j", capsule: "other" }));
        assert!(!s.allows(&Action::AdminGlobal));
    }

    #[test]
    fn token_issue_lookup_roundtrip() {
        let tmp = TempDir::new().unwrap();
        let mut store = TokenStore::load_or_init(tmp.path());
        let (raw, hash) = store.issue(
            Scope::Admin, None, "test".into(), now(),
        ).unwrap();
        assert_eq!(raw.len(), 64);
        assert_eq!(hash.len(), 64);
        let rec = store.lookup(&raw, now()).unwrap();
        assert!(matches!(rec.scope, Scope::Admin));
    }

    #[test]
    fn token_revoke_removes_lookup() {
        let tmp = TempDir::new().unwrap();
        let mut store = TokenStore::load_or_init(tmp.path());
        let (raw, hash) = store.issue(
            Scope::Admin, None, "test".into(), now(),
        ).unwrap();
        assert!(store.lookup(&raw, now()).is_some());
        let removed = store.revoke(&hash).unwrap();
        assert!(removed);
        assert!(store.lookup(&raw, now()).is_none());
    }

    #[test]
    fn token_expires_after_ttl() {
        let tmp = TempDir::new().unwrap();
        let mut store = TokenStore::load_or_init(tmp.path());
        let (raw, _) = store.issue(
            Scope::Admin, Some(60), "short".into(), 1000,
        ).unwrap();
        // Inside the TTL.
        assert!(store.lookup(&raw, 1030).is_some());
        // Past the TTL.
        assert!(store.lookup(&raw, 1100).is_none());
    }

    #[test]
    fn token_store_persists_across_reload() {
        let tmp = TempDir::new().unwrap();
        let raw = {
            let mut s = TokenStore::load_or_init(tmp.path());
            let (raw, _) = s.issue(
                Scope::TenantAdmin { tenant: "acme".into() },
                None, "team".into(), now()).unwrap();
            raw
        };
        let reopened = TokenStore::load_or_init(tmp.path());
        let rec = reopened.lookup(&raw, now()).unwrap();
        assert_eq!(rec.label, "team");
        assert!(matches!(&rec.scope, Scope::TenantAdmin { tenant } if tenant == "acme"));
    }

    #[test]
    fn malformed_tokens_file_is_treated_as_empty() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("tokens.json"), "not json").unwrap();
        let store = TokenStore::load_or_init(tmp.path());
        assert!(store.tokens.is_empty());
    }

    #[test]
    fn token_format_is_64_lowercase_hex() {
        let t = generate_token();
        assert_eq!(t.len(), 64, "token must be exactly 64 chars, got {}", t.len());
        for c in t.chars() {
            assert!(
                c.is_ascii_digit() || ('a'..='f').contains(&c),
                "token char {c:?} is not in 0-9a-f"
            );
        }
    }

    #[test]
    fn tokens_are_unique() {
        use std::collections::HashSet;
        let n = 1000;
        let set: HashSet<String> = (0..n).map(|_| generate_token()).collect();
        assert_eq!(set.len(), n, "expected {n} unique tokens, got {}", set.len());
    }

    #[test]
    fn tokens_have_high_entropy() {
        // 100 tokens × 32 bytes = 3200 bytes total. Uniform expectation is
        // ~12.5 per byte value (3200 / 256). A non-CSPRNG would skew this
        // heavily; a true CSPRNG should keep every count well under ~10×
        // the mean. We use a conservative upper bound to stay flake-free.
        let n_tokens = 100;
        let total_bytes = n_tokens * 32;
        let expected_mean = total_bytes as f64 / 256.0;
        let mut counts = [0u32; 256];
        for _ in 0..n_tokens {
            let t = generate_token();
            let bytes = t.as_bytes();
            // Each token is 64 hex chars; decode back to 32 bytes for histogram.
            for chunk in bytes.chunks(2) {
                let hi = (chunk[0] as char).to_digit(16).unwrap() as u8;
                let lo = (chunk[1] as char).to_digit(16).unwrap() as u8;
                counts[((hi << 4) | lo) as usize] += 1;
            }
        }
        let max = *counts.iter().max().unwrap() as f64;
        // No byte value should appear more than 8× the expected mean.
        assert!(
            max <= expected_mean * 8.0,
            "byte distribution looks non-uniform: max={max}, mean={expected_mean}"
        );
    }
}
