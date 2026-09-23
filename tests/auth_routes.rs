//! Authentication and authorization: a route-by-scope matrix over every
//! route in `src/server/routes.rs`, token issue/list/revoke/expiry, the
//! `Ocp-Apim-Subscription-Key` header, tenant isolation in listings, and
//! dev mode (admin without a key, loopback binds only).

mod common;

use std::collections::HashMap;
use std::time::Duration;

use common::*;
use serde_json::{Value, json};

const ADMIN_KEY: &str = "operator-key-for-auth-tests";

// ───────────────────────────── the matrix ────────────────────────────────

/// What a route requires.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Class {
    /// No credential at all: health, readiness, metrics, console.
    Open,
    /// Any valid credential.
    AnyAuth,
    /// The operator (admin scope).
    AdminOnly,
    /// `TenantOp { tenant }`.
    Tenant,
    /// `CapsuleRead`.
    CapRead,
    /// `CapsuleDecide`: decide and reward.
    CapData,
    /// `CapsuleMutate`: spec, mode, program, policy, deletes.
    CapMutate,
}

#[derive(Clone, Copy)]
enum Body {
    None,
    Json(&'static str),
    Program,
    /// `{"decisionId": <the target's fixture decision>, "reward": 1}`.
    Reward,
}

struct Route {
    method: &'static str,
    /// `{t}`, `{j}`, `{c}`, `{id}` (fixture decision), `{hash}` (a
    /// throwaway token) are filled in per call.
    path: String,
    body: Body,
    class: Class,
    /// Statuses an authorized call must get.
    ok: &'static [u16],
    /// An authorized call removes fixture state.
    destructive: bool,
}

fn r(
    method: &'static str,
    path: impl Into<String>,
    body: Body,
    class: Class,
    ok: &'static [u16],
    destructive: bool,
) -> Route {
    Route {
        method,
        path: path.into(),
        body,
        class,
        ok,
        destructive,
    }
}

const CAP: &str = "/v1/tenants/{t}/jobs/{j}/capsules/{c}";

/// Every route in `src/server/routes.rs`.
fn routes() -> Vec<Route> {
    use Body as B;
    use Class::*;
    let cap = |tail: &str| format!("{CAP}{tail}");
    vec![
        r("GET", "/health", B::None, Open, &[200], false),
        r("GET", "/ready", B::None, Open, &[200], false),
        r("GET", "/metrics", B::None, AdminOnly, &[200], false),
        r("GET", "/admin", B::None, Open, &[200], false),
        r("GET", "/v1/admin", B::None, Open, &[200], false),
        r("GET", "/v1/auth/whoami", B::None, AnyAuth, &[200], false),
        r("GET", "/v1/capabilities", B::None, AnyAuth, &[200], false),
        r("GET", "/v1/tenants", B::None, AnyAuth, &[200], false),
        r(
            "POST",
            "/v1/admin/tokens",
            B::Json(r#"{"scope":{"kind":"admin"},"label":"matrix"}"#),
            AdminOnly,
            &[200],
            false,
        ),
        r("GET", "/v1/admin/tokens", B::None, AdminOnly, &[200], false),
        r(
            "DELETE",
            "/v1/admin/tokens/{hash}",
            B::None,
            AdminOnly,
            &[200],
            false,
        ),
        r(
            "GET",
            "/v1/admin/capsules",
            B::None,
            AdminOnly,
            &[200],
            false,
        ),
        r("DELETE", "/v1/tenants/{t}", B::None, Tenant, &[200], true),
        r(
            "GET",
            "/v1/tenants/{t}/jobs",
            B::None,
            Tenant,
            &[200],
            false,
        ),
        r(
            "POST",
            "/v1/tenants/{t}/jobs",
            B::Json(r#"{"id":"extra"}"#),
            Tenant,
            &[200, 201],
            false,
        ),
        r(
            "GET",
            "/v1/tenants/{t}/jobs/{j}",
            B::None,
            Tenant,
            &[200],
            false,
        ),
        r(
            "DELETE",
            "/v1/tenants/{t}/jobs/{j}",
            B::None,
            Tenant,
            &[200],
            true,
        ),
        r(
            "GET",
            "/v1/tenants/{t}/jobs/{j}/capsules",
            B::None,
            Tenant,
            &[200],
            false,
        ),
        r(
            "POST",
            cap("/decide"),
            B::Json(r#"{"context":{"x":1}}"#),
            CapData,
            &[200],
            false,
        ),
        r("POST", cap("/reward"), B::Reward, CapData, &[200], false),
        r("POST", cap("/feedback"), B::Reward, CapData, &[200], false),
        // SDK uploads are data-plane, like decide and reward.
        r(
            "POST",
            cap("/decisions:batch"),
            B::Json(r#"{"decisions":[]}"#),
            CapData,
            &[200],
            false,
        ),
        r(
            "POST",
            cap("/rewards:batch"),
            B::Json(r#"{"rewards":[]}"#),
            CapData,
            &[200],
            false,
        ),
        r("GET", cap(""), B::None, CapRead, &[200], false),
        r("GET", cap("/spec"), B::None, CapRead, &[200], false),
        r("GET", cap("/policy"), B::None, CapRead, &[200], false),
        r("GET", cap("/decisions"), B::None, CapRead, &[200], false),
        r(
            "GET",
            cap("/decisions/{id}"),
            B::None,
            CapRead,
            &[200],
            false,
        ),
        r("GET", cap("/model"), B::None, CapRead, &[200], false),
        r("GET", cap("/audits"), B::None, CapRead, &[200], false),
        // Off-policy evaluation is expensive, so it takes a tenant-admin
        // credential; with one logged decision it may answer 409 (nothing
        // to evaluate), which is still authorized.
        r(
            "POST",
            cap("/evaluate"),
            B::Json(r#"{"policy":"logged"}"#),
            CapMutate,
            &[200, 409],
            false,
        ),
        // Promotion changes the spec.
        r(
            "POST",
            cap("/promote"),
            B::Json(r#"{"spec":{"exploration":{"floor":0.05}},"gates":["lift.dr.lower >= -1"]}"#),
            CapMutate,
            &[200, 409],
            false,
        ),
        r(
            "PUT",
            cap("/spec"),
            B::Json(r#"{"exploration":{"floor":0.05}}"#),
            CapMutate,
            &[200],
            false,
        ),
        r(
            "POST",
            cap("/mode"),
            B::Json(r#"{"mode":"learner"}"#),
            CapMutate,
            &[200],
            false,
        ),
        r(
            "POST",
            cap("/install"),
            B::Program,
            CapMutate,
            &[200],
            false,
        ),
        r("DELETE", cap("/program"), B::None, CapMutate, &[200], false),
        r(
            "PUT",
            cap("/policy"),
            B::Json(r#"{"allow_stdout":false}"#),
            CapMutate,
            &[200],
            false,
        ),
        r("DELETE", cap("/logs"), B::None, CapMutate, &[200], true),
        r("DELETE", cap(""), B::None, CapMutate, &[200], true),
        // Unknown routes answer 404 once authenticated.
        r("GET", "/v1/no/such/route", B::None, AnyAuth, &[404], false),
        r(
            "GET",
            cap("/no-such-route"),
            B::None,
            AnyAuth,
            &[404],
            false,
        ),
    ]
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Principal {
    OperatorKey,
    AdminToken,
    /// `tenant_admin` for `acme`.
    TenantAcme,
    /// `read` for `acme/j/c`.
    ReadAcmeC,
    NoCredential,
    WrongKey,
}

const PRINCIPALS: [Principal; 6] = [
    Principal::OperatorKey,
    Principal::AdminToken,
    Principal::TenantAcme,
    Principal::ReadAcmeC,
    Principal::NoCredential,
    Principal::WrongKey,
];

#[derive(Debug, PartialEq)]
enum Expect {
    Allow,
    Forbidden,
    Unauthorized,
}

fn expect(p: Principal, class: Class, (t, j, c): (&str, &str, &str)) -> Expect {
    use Class::*;
    use Principal::*;
    if class == Open {
        return Expect::Allow;
    }
    let allowed = match p {
        NoCredential | WrongKey => return Expect::Unauthorized,
        OperatorKey | AdminToken => true,
        TenantAcme => match class {
            AdminOnly => false,
            AnyAuth => true,
            _ => t == "acme",
        },
        ReadAcmeC => match class {
            AnyAuth => true,
            CapRead | CapData => (t, j, c) == ("acme", "j", "c"),
            _ => false,
        },
    };
    if allowed {
        Expect::Allow
    } else {
        Expect::Forbidden
    }
}

/// Where a route is aimed: the read token's capsule, a sibling capsule in
/// the same tenant, and a capsule in another tenant.
const TARGETS: [(&str, &str, &str); 3] = [
    ("acme", "j", "c"),
    ("acme", "j", "c2"),
    ("globex", "j", "c"),
];

struct Fixture {
    app: App,
    admin_token: String,
    tenant_token: String,
    read_token: String,
    decisions: HashMap<(String, String, String), String>,
    program: Vec<u8>,
    dirty: bool,
}

impl Fixture {
    fn new(label: &str) -> Fixture {
        let app = App::with_key(label, ADMIN_KEY);
        let (admin_token, _) = app.issue_token(json!({"kind": "admin"}), None);
        let (tenant_token, _) =
            app.issue_token(json!({"kind": "tenant_admin", "tenant": "acme"}), None);
        let (read_token, _) = app.issue_token(
            json!({"kind": "read", "tenant": "acme", "job": "j", "capsule": "c"}),
            None,
        );
        let mut fx = Fixture {
            app,
            admin_token,
            tenant_token,
            read_token,
            decisions: HashMap::new(),
            program: compile_lycan("(!cap \"runtime.publish\" \"reason\" \"ok\")"),
            dirty: true,
        };
        fx.reset();
        fx
    }

    /// Every target capsule exists with one committed decision.
    fn reset(&mut self) {
        for (t, j, c) in TARGETS {
            self.app.put_spec(
                t,
                j,
                c,
                json!({"actions": [{"id": "a"}, {"id": "b"}], "learner": {"bits": 10}}),
            );
            let d = self
                .app
                .decide(t, j, c, json!({"context": {}, "durable": true}));
            self.decisions.insert(
                (t.into(), j.into(), c.into()),
                d["decisionId"].as_str().unwrap().to_string(),
            );
        }
        self.dirty = false;
    }

    fn secret(&self, p: Principal) -> Option<String> {
        match p {
            Principal::OperatorKey => Some(ADMIN_KEY.into()),
            Principal::AdminToken => Some(self.admin_token.clone()),
            Principal::TenantAcme => Some(self.tenant_token.clone()),
            Principal::ReadAcmeC => Some(self.read_token.clone()),
            Principal::NoCredential => None,
            Principal::WrongKey => Some("f".repeat(64)),
        }
    }

    fn cred(&self, p: Principal, subscription_key: bool) -> Cred {
        match (self.secret(p), subscription_key) {
            (None, _) => Cred::None,
            (Some(s), false) => Cred::Bearer(s),
            (Some(s), true) => Cred::SubscriptionKey(s),
        }
    }
}

fn run_matrix(subscription_key: bool) {
    let mut fx = Fixture::new(if subscription_key {
        "matrix-ocp"
    } else {
        "matrix-bearer"
    });
    let mut failures = Vec::new();
    let mut calls = 0;
    for route in routes() {
        let targets: Vec<(&str, &str, &str)> = if route.path.contains("{c}") {
            TARGETS.to_vec()
        } else if route.path.contains("{t}") {
            vec![("acme", "j", "-"), ("globex", "j", "-")]
        } else {
            vec![("-", "-", "-")]
        };
        for target in targets {
            for p in PRINCIPALS {
                if fx.dirty {
                    fx.reset();
                }
                let (t, j, c) = target;
                let id = fx
                    .decisions
                    .get(&(t.to_string(), j.to_string(), c.to_string()))
                    .cloned()
                    .unwrap_or_default();
                let mut path = route
                    .path
                    .replace("{t}", t)
                    .replace("{j}", j)
                    .replace("{c}", c)
                    .replace("{id}", &id);
                if path.contains("{hash}") {
                    let (_, hash) = fx.app.issue_token(json!({"kind": "admin"}), None);
                    path = path.replace("{hash}", &hash);
                }
                let body: Option<Vec<u8>> = match route.body {
                    Body::None => None,
                    Body::Json(s) => Some(s.as_bytes().to_vec()),
                    Body::Program => Some(fx.program.clone()),
                    Body::Reward => Some(
                        json!({"decisionId": id, "reward": 1})
                            .to_string()
                            .into_bytes(),
                    ),
                };
                let resp = fx.app.raw(
                    route.method,
                    &path,
                    &fx.cred(p, subscription_key),
                    body.as_deref(),
                );
                calls += 1;
                let text = String::from_utf8_lossy(&resp.body).into_owned();
                let want = expect(p, route.class, target);
                let fine = match want {
                    Expect::Allow => route.ok.contains(&resp.status),
                    Expect::Forbidden => {
                        resp.status == 403
                            && parse_body(&resp.body)
                                == json!({"error": "forbidden: scope does not allow this action"})
                    }
                    Expect::Unauthorized => {
                        resp.status == 401
                            && parse_body(&resp.body) == json!({"error": "unauthorized"})
                    }
                };
                if !fine {
                    failures.push(format!(
                        "{p:?} {} {path}: expected {want:?} {:?}, got {} {text}",
                        route.method, route.ok, resp.status
                    ));
                }
                if want == Expect::Allow && route.destructive {
                    fx.dirty = true;
                }
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {calls} calls broke the matrix:\n{}",
        failures.len(),
        failures.join("\n")
    );
    assert_eq!(calls, 546, "42 routes x targets x 6 principals");
}

#[test]
fn route_scope_matrix_with_bearer_tokens() {
    run_matrix(false);
}

#[test]
fn route_scope_matrix_with_subscription_key_header() {
    run_matrix(true);
}

// ───────────────────────────── narratives ────────────────────────────────

#[test]
fn read_tokens_decide_reward_and_read_but_never_change_their_capsule() {
    let fx = Fixture::new("read-scope");
    let app = &fx.app;
    let read = Cred::Bearer(fx.read_token.clone());
    let path = |tail: &str| cap("acme", "j", "c", tail);

    let d = app.raw(
        "POST",
        &path("/decide"),
        &read,
        Some(br#"{"context":{"u":1}}"#),
    );
    assert_eq!(d.status, 200);
    let id = parse_body(&d.body)["decisionId"]
        .as_str()
        .unwrap()
        .to_string();
    let rw = app.raw(
        "POST",
        &path("/reward"),
        &read,
        Some(
            json!({"decisionId": id, "reward": 1, "durable": true})
                .to_string()
                .as_bytes(),
        ),
    );
    assert_eq!(rw.status, 200, "{}", String::from_utf8_lossy(&rw.body));
    for tail in ["", "/spec", "/policy", "/model", "/decisions", "/audits"] {
        assert_eq!(
            app.raw("GET", &path(tail), &read, None).status,
            200,
            "GET {tail}"
        );
    }
    let rec = app.raw("GET", &path(&format!("/decisions/{id}")), &read, None);
    assert_eq!(parse_body(&rec.body)["rewards"][0]["reward"], json!(1.0));
    // SDK uploads and off-policy evaluation are allowed too.
    for (tail, body) in [
        ("/decisions:batch", &br#"{"decisions":[]}"#[..]),
        ("/rewards:batch", &br#"{"rewards":[]}"#[..]),
    ] {
        let r = app.raw("POST", &path(tail), &read, Some(body));
        assert_eq!(
            r.status,
            200,
            "{tail}: {}",
            String::from_utf8_lossy(&r.body)
        );
    }
    // Evaluation is expensive: not for data-plane keys.
    let ev = app.raw(
        "POST",
        &path("/evaluate"),
        &read,
        Some(br#"{"policy":"logged"}"#),
    );
    assert_eq!(
        ev.status,
        403,
        "evaluate: {}",
        String::from_utf8_lossy(&ev.body)
    );

    let spec_before = app.ok("GET", &path("/spec"), None, 200);
    let policy_before = app.ok("GET", &path("/policy"), None, 200);
    let audits_before = app.ok("GET", &path("/audits"), None, 200);
    for (method, tail, body) in [
        ("PUT", "/spec", Some(br#"{"mode":"frozen"}"#.to_vec())),
        ("POST", "/mode", Some(br#"{"mode":"frozen"}"#.to_vec())),
        ("POST", "/install", Some(fx.program.clone())),
        ("DELETE", "/program", None),
        (
            "PUT",
            "/policy",
            Some(br#"{"allow_network":true,"allowed_hosts":["a.example"]}"#.to_vec()),
        ),
        ("DELETE", "/logs", None),
        ("DELETE", "", None),
        (
            "POST",
            "/promote",
            Some(br#"{"spec":{"mode":"frozen"},"gates":["lift.dr.lower >= -1"]}"#.to_vec()),
        ),
    ] {
        let resp = app.raw(method, &path(tail), &read, body.as_deref());
        assert_eq!(resp.status, 403, "{method} {tail}");
    }
    // Nothing changed.
    assert_eq!(app.ok("GET", &path("/spec"), None, 200), spec_before);
    assert_eq!(app.ok("GET", &path("/policy"), None, 200), policy_before);
    assert_eq!(app.ok("GET", &path("/audits"), None, 200), audits_before);
    let capsule = app.ok("GET", &path(""), None, 200);
    assert_eq!(capsule["stats"]["rewards"], json!(1));
    assert!(capsule["program"].is_null());

    // Its scope names exactly one capsule.
    for other in [
        cap("acme", "j", "c2", "/decide"),
        cap("globex", "j", "c", "/decide"),
        cap("acme", "j2", "c", "/decide"),
    ] {
        assert_eq!(
            app.raw("POST", &other, &read, Some(b"{}")).status,
            403,
            "{other}"
        );
    }
    // And it can see no tenant, list no job, and administer nothing.
    assert_eq!(
        parse_body(&app.raw("GET", "/v1/tenants", &read, None).body)["tenants"],
        json!([])
    );
    for (m, target) in [
        ("GET", "/v1/tenants/acme/jobs"),
        ("GET", "/v1/admin/tokens"),
        ("GET", "/v1/admin/capsules"),
    ] {
        assert_eq!(app.raw(m, target, &read, None).status, 403, "{target}");
    }
}

#[test]
fn tenant_listings_are_isolated() {
    let fx = Fixture::new("isolation");
    let app = &fx.app;
    let tenant = Cred::Bearer(fx.tenant_token.clone());
    let admin = Cred::Bearer(ADMIN_KEY.into());
    let tenants = |cred: &Cred| {
        parse_body(&app.raw("GET", "/v1/tenants", cred, None).body)["tenants"].clone()
    };
    assert_eq!(tenants(&admin), json!(["acme", "globex"]));
    assert_eq!(tenants(&tenant), json!(["acme"]));

    let jobs = app.raw("GET", "/v1/tenants/acme/jobs", &tenant, None);
    assert_eq!(jobs.status, 200);
    assert_eq!(
        parse_body(&jobs.body)["jobs"][0]["capsules"],
        json!(["c", "c2"])
    );
    for target in [
        "/v1/tenants/globex/jobs",
        "/v1/tenants/globex/jobs/j",
        "/v1/tenants/globex/jobs/j/capsules",
        "/v1/admin/capsules",
    ] {
        let resp = app.raw("GET", target, &tenant, None);
        assert_eq!(resp.status, 403, "{target}");
        assert!(
            !String::from_utf8_lossy(&resp.body).contains("globex"),
            "{target}"
        );
    }
    // A tenant admin can neither create jobs in, nor delete, another tenant.
    assert_eq!(
        app.raw(
            "POST",
            "/v1/tenants/globex/jobs",
            &tenant,
            Some(br#"{"id":"x"}"#)
        )
        .status,
        403
    );
    assert_eq!(
        app.raw("DELETE", "/v1/tenants/globex", &tenant, None)
            .status,
        403
    );
    assert_eq!(tenants(&admin), json!(["acme", "globex"]));
    assert!(app.state.store.list_jobs("globex").unwrap().len() == 1);
}

// ───────────────────────────── tokens ────────────────────────────────────

#[test]
fn tokens_are_issued_listed_used_and_revoked() {
    let mut app = App::with_key("tokens", ADMIN_KEY);
    let scope = json!({"kind": "tenant_admin", "tenant": "acme"});
    let issued = app.ok(
        "POST",
        "/v1/admin/tokens",
        Some(json!({"scope": scope, "label": "ci-deployer"})),
        200,
    );
    let token = issued["token"].as_str().unwrap().to_string();
    let hash = issued["hash"].as_str().unwrap().to_string();
    assert!(
        token.len() == 64 && token.chars().all(|c| c.is_ascii_hexdigit()),
        "{token}"
    );
    assert_eq!(hash, syntra::store::sha256_hex(token.as_bytes()));
    assert_eq!(issued["scope"], scope);
    assert!(issued["expiresAt"].is_null());

    // Listed by hash only; the raw token is never shown or stored again.
    let listed = app.ok("GET", "/v1/admin/tokens", None, 200);
    let entry = listed["tokens"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["hash"] == json!(hash))
        .expect("listed")
        .clone();
    assert_eq!(entry["label"], json!("ci-deployer"));
    assert_eq!(entry["scope"], scope);
    assert!(entry["createdAt"].as_u64().unwrap() > 1_700_000_000);
    assert!(entry["lastUsedAt"].is_null());
    assert!(!listed.to_string().contains(&token));
    let on_disk = std::fs::read_to_string(app.store().join("tokens.json")).unwrap();
    assert!(on_disk.contains(&hash) && !on_disk.contains(&token));

    // It authenticates as a scoped token and its use is recorded.
    let who = app.raw("GET", "/v1/auth/whoami", &Cred::Bearer(token.clone()), None);
    assert_eq!(who.status, 200);
    assert_eq!(
        parse_body(&who.body),
        json!({"ok": true, "kind": "scoped_token", "principalId": hash, "scope": scope})
    );
    let listed = app.ok("GET", "/v1/admin/tokens", None, 200);
    let entry = listed["tokens"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["hash"] == json!(hash))
        .unwrap()
        .clone();
    assert!(entry["lastUsedAt"].as_u64().is_some(), "{entry}");

    // Tokens survive a restart.
    app.restart(true);
    assert_eq!(
        app.raw("GET", "/v1/auth/whoami", &Cred::Bearer(token.clone()), None)
            .status,
        200
    );

    // Revoked: rejected at once, gone from the list, and a second revoke is
    // 404.
    let v = app.ok("DELETE", &format!("/v1/admin/tokens/{hash}"), None, 200);
    assert_eq!(v, json!({"ok": true, "revoked": true}));
    assert_eq!(
        app.raw("GET", "/v1/auth/whoami", &Cred::Bearer(token.clone()), None)
            .status,
        401
    );
    let listed = app.ok("GET", "/v1/admin/tokens", None, 200);
    assert!(!listed.to_string().contains(&hash));
    assert_eq!(
        app.call("DELETE", &format!("/v1/admin/tokens/{hash}"), None)
            .0,
        404
    );

    // Bad requests.
    for body in [
        json!({}),
        json!({"scope": {"kind": "superuser"}}),
        json!({"scope": {"kind": "read", "tenant": "acme"}}),
        json!({"scope": "admin"}),
    ] {
        let (st, v) = app.call("POST", "/v1/admin/tokens", Some(body.clone()));
        assert_eq!(st, 400, "{body}: {v}");
    }
}

#[test]
fn tokens_expire_after_their_ttl() {
    let app = App::with_key("ttl", ADMIN_KEY);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();

    // TTL 0: expired on issue.
    let (dead, dead_hash) = app.issue_token(json!({"kind": "admin"}), Some(0));
    assert_eq!(
        app.raw("GET", "/v1/auth/whoami", &Cred::Bearer(dead), None)
            .status,
        401
    );
    assert!(
        !app.ok("GET", "/v1/admin/tokens", None, 200)
            .to_string()
            .contains(&dead_hash)
    );

    // A longer TTL reports its expiry.
    let v = app.ok(
        "POST",
        "/v1/admin/tokens",
        Some(json!({"scope": {"kind": "admin"}, "ttlSeconds": 3600})),
        200,
    );
    let at = v["expiresAt"].as_u64().unwrap();
    assert!((now + 3600..=now + 3602).contains(&at), "{v}");

    // TTL 2: works now, then stops working on its own.
    let (short, short_hash) = app.issue_token(json!({"kind": "admin"}), Some(2));
    let cred = Cred::Bearer(short);
    assert_eq!(app.raw("GET", "/v1/auth/whoami", &cred, None).status, 200);
    assert!(
        wait_until(Duration::from_secs(6), || app
            .raw("GET", "/v1/auth/whoami", &cred, None)
            .status
            == 401),
        "a 2 s token still works after 6 s"
    );
    assert!(
        !app.ok("GET", "/v1/admin/tokens", None, 200)
            .to_string()
            .contains(&short_hash)
    );
}

#[test]
fn subscription_key_header_works_like_bearer() {
    let app = App::with_key("ocp", ADMIN_KEY);
    let (token, hash) = app.issue_token(json!({"kind": "admin"}), None);
    let whoami = |cred: Cred| {
        let r = app.raw("GET", "/v1/auth/whoami", &cred, None);
        (r.status, parse_body(&r.body))
    };
    let (st, v) = whoami(Cred::SubscriptionKey(ADMIN_KEY.into()));
    assert_eq!(st, 200);
    assert_eq!(v["kind"], json!("legacy_admin"));
    assert_eq!(v["scope"], json!({"kind": "admin"}));
    let (st, v) = whoami(Cred::SubscriptionKey(token.clone()));
    assert_eq!(st, 200);
    assert_eq!(v["kind"], json!("scoped_token"));
    assert_eq!(v["principalId"], json!(hash));
    // Surrounding whitespace is ignored, as for Bearer.
    let (st, _) = whoami(Cred::Header(
        "Ocp-Apim-Subscription-Key".into(),
        format!("  {token} "),
    ));
    assert_eq!(st, 200);
    let (st, _) = whoami(Cred::SubscriptionKey("wrong".into()));
    assert_eq!(st, 401);
}

#[test]
fn malformed_or_wrong_credentials_are_401() {
    let app = App::with_key("badcreds", ADMIN_KEY);
    for cred in [
        Cred::None,
        Cred::Bearer("not-the-key".into()),
        Cred::Bearer(format!("{ADMIN_KEY}x")),
        Cred::Bearer(ADMIN_KEY[..ADMIN_KEY.len() - 1].into()),
        Cred::Header("Authorization".into(), "Bearer ".into()),
        Cred::Header("Authorization".into(), ADMIN_KEY.into()),
        Cred::Header("Authorization".into(), format!("Basic {ADMIN_KEY}")),
        Cred::Header("Authorization".into(), format!("Token {ADMIN_KEY}")),
        Cred::SubscriptionKey(String::new()),
    ] {
        for target in [
            "/v1/auth/whoami",
            "/v1/tenants",
            "/tenants",
            "/v1/admin/tokens",
        ] {
            let r = app.raw("GET", target, &cred, None);
            assert_eq!(r.status, 401, "{cred:?} {target}");
            assert_eq!(parse_body(&r.body), json!({"error": "unauthorized"}));
        }
    }
    // The operator key itself works, as Bearer.
    let r = app.raw(
        "GET",
        "/v1/auth/whoami",
        &Cred::Bearer(ADMIN_KEY.into()),
        None,
    );
    assert_eq!(parse_body(&r.body)["kind"], json!("legacy_admin"));
}

// ───────────────────────────── dev mode ──────────────────────────────────

#[test]
fn dev_mode_grants_admin_only_on_a_loopback_bind() {
    // On loopback, every request is the operator, without a credential.
    let dir = TempDir::new("devmode");
    let srv = Server::start(&dir.join("store"), None);
    let a = agent();
    let get = |target: &str| try_http(&a, "GET", &srv.url(target), &[], None).unwrap();
    let who = get("/v1/auth/whoami").json();
    assert_eq!(who["kind"], json!("dev_mode"));
    assert_eq!(who["scope"], json!({"kind": "admin"}));
    assert!(who["principalId"].is_null());
    assert_eq!(get("/v1/admin/tokens").status, 200);
    let r = try_http(
        &a,
        "PUT",
        &srv.url(&cap("acme", "j", "c", "/spec")),
        &[],
        Some(br#"{"actions":[{"id":"a"}]}"#),
    )
    .unwrap();
    assert_eq!(r.status, 201, "{}", r.body);

    // Anything that is not loopback is refused before binding.
    for addr in [
        "0.0.0.0:0",
        "[::]:0",
        "192.0.2.10:8787",
        "example.com:80",
        ":8787",
    ] {
        let store = dir.join(&format!("refused-{}", unique()));
        let out = run(
            SYNTRA,
            &[
                "serve",
                "--dev-mode",
                "--addr",
                addr,
                "--store",
                store.to_str().unwrap(),
            ],
        );
        assert_eq!(out.status.code(), Some(1), "{addr}: {}", stderr(&out));
        let err = stderr(&out);
        assert!(
            err.contains("only binds a loopback address")
                && err.contains("--dev-mode-allow-remote"),
            "{addr}: {err}"
        );
        assert!(!store.exists(), "{addr}: refused before touching the store");
    }
}

#[test]
fn a_key_always_wins_over_dev_mode_and_no_key_is_refused() {
    // `--dev-mode` next to a key: the key is required.
    let dir = TempDir::new("devkey");
    let srv = Server::start_with(&dir.join("store"), Some("k-both"), &["--dev-mode"]);
    let a = agent();
    let url = srv.url("/v1/auth/whoami");
    assert_eq!(try_http(&a, "GET", &url, &[], None).unwrap().status, 401);
    let v: Value = try_http(&a, "GET", &url, &[("Authorization", "Bearer k-both")], None)
        .unwrap()
        .json();
    assert_eq!(v["kind"], json!("legacy_admin"));
    drop(srv);

    // Neither a key nor --dev-mode: refuses to start (fail closed).
    let out = run(
        SYNTRA,
        &[
            "serve",
            "--addr",
            "127.0.0.1:0",
            "--store",
            dir.join("nokey").to_str().unwrap(),
        ],
    );
    assert_eq!(out.status.code(), Some(1));
    assert!(
        stderr(&out).contains("no admin key set"),
        "{}",
        stderr(&out)
    );
    assert!(!dir.join("nokey").exists());
}
