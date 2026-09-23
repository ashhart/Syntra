//! The admin console: one static page (`console.html`). It holds no data
//! itself; every call goes through the authenticated API with the key the
//! operator enters, kept in the tab's session storage.
//!
//! The page is served with a Content-Security-Policy that allows only its
//! own inline script and style (by SHA-256 hash) and requests to this
//! origin, so markup that slipped past escaping could not run script.

use std::sync::OnceLock;

use sha2::{Digest, Sha256};

const CONSOLE: &str = include_str!("console.html");

pub fn console_html(service_name: &str) -> String {
    CONSOLE.replace("{{SERVICE}}", &html_escape(service_name))
}

/// The Content-Security-Policy for [`console_html`].
pub fn console_csp() -> &'static str {
    static CSP: OnceLock<String> = OnceLock::new();
    CSP.get_or_init(|| {
        let hash = |open: &str, close: &str| {
            let start = CONSOLE.find(open).expect("console tag") + open.len();
            let end = start + CONSOLE[start..].find(close).expect("console closing tag");
            let digest = Sha256::digest(CONSOLE[start..end].as_bytes());
            format!("'sha256-{}'", super::query::base64_encode(&digest))
        };
        format!(
            "default-src 'none'; script-src {}; style-src {}; connect-src 'self'; \
             img-src 'self' data:; base-uri 'none'; form-action 'none'; frame-ancestors 'none'",
            hash("<script>", "</script>"),
            hash("<style>", "</style>"),
        )
    })
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_console_is_escaped_and_locked_down() {
        let html = console_html("<b>x</b>");
        assert!(html.contains("&lt;b&gt;x&lt;/b&gt;"));
        assert!(!html.contains("<b>x</b>"));
        let csp = console_csp();
        assert!(csp.contains("script-src 'sha256-"), "{csp}");
        assert!(csp.contains("frame-ancestors 'none'"), "{csp}");
        assert!(!csp.contains("unsafe-inline"), "{csp}");
        // One script and one style block, so the hashes cover everything.
        assert_eq!(CONSOLE.matches("<script>").count(), 1);
        assert_eq!(CONSOLE.matches("<style>").count(), 1);
        assert!(!CONSOLE.contains(" onclick=") && !CONSOLE.contains(" onchange="));
    }
}
