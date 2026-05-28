// Copyright 2026 Ash Hart. Apache-2.0.

//! Minimal end-to-end example for the Syntra retry client.
//!
//! Run a Syntra appliance at `SYNTRA_URL` (default `http://localhost:8787`)
//! with the demo retry-tuning capsule installed, then:
//!
//! ```text
//! SYNTRA_ADMIN_KEY=... cargo run --example retry_basic --release
//! ```

use std::env;

use syntra_client::retry::RetryClient;
use syntra_client::SyntraClient;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let url = env::var("SYNTRA_URL").unwrap_or_else(|_| "http://localhost:8787".into());
    let admin_key = env::var("SYNTRA_ADMIN_KEY").unwrap_or_default();
    let capsule = env::var("SYNTRA_CAPSULE_PATH")
        .unwrap_or_else(|_| "/tenants/demo/jobs/retry/capsules/router".into());
    let target = env::var("TARGET_URL").unwrap_or_else(|_| "https://httpbin.org/status/200".into());

    let syntra = SyntraClient::new(url, admin_key, capsule);
    let client = RetryClient::builder(syntra)
        .on_feedback_error(|e| eprintln!("feedback error: {e}"))
        .build();

    let resp = client.get(&target)?;
    println!("status: {}", resp.status());
    Ok(())
}
