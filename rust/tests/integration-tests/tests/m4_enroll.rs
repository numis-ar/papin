//! Enrollment (§7.1) and status endpoint tests, using a stub enroll helper
//! (the real setuid helper is exercised in operations.md on a deployed host).

use http::{header, Request, StatusCode};
use integration_tests::common::*;
use serde_json::{json, Value};
use std::io::Write;
use std::sync::Arc;
use tower::ServiceExt;

const PUBKEY_A: &str = "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8=";
const PUBKEY_B: &str = "AQIDBAUGBwgJCgsMDQ4PEBESExQVFhcYGRobHB0eHyA=";

/// Stub helper: records invocations, prints ok. Stands in for the setuid
/// binary so the gateway enroll flow is testable without root.
fn stub_helper(dir: &std::path::Path) -> std::path::PathBuf {
    let path = dir.join("stub-enroll-helper.sh");
    let mut f = std::fs::File::create(&path).unwrap();
    write!(
        f,
        "#!/bin/bash\necho \"$@\" >> \"{}/helper.log\"\necho ok\n",
        dir.display()
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    path
}

fn enroll_state() -> (Arc<papin_gateway::AppState>, TestDirs, std::path::PathBuf) {
    let dirs = test_dirs();
    let catalog = test_catalog();
    let helper = stub_helper(dirs._tmp.path());
    let state = papin_gateway::server::make_state_full(
        Arc::new(papin_gateway::FakeProvider::new(
            dirs.registry_dir.clone(),
            fake_script(),
            catalog.clone(),
        )),
        Arc::new(papin_gateway::FakeBootstrap::new(
            dirs.agents_root.clone(),
            dirs.seeds_dir.clone(),
        )),
        catalog,
        std::time::Duration::from_secs(600),
        token_store(),
        papin_gateway::EnrollConfig {
            peers_dir: dirs._tmp.path().join("wg-peers"),
            gateway_ip: "10.77.0.1".into(),
            client_pool: "10.77.0.0/24".into(),
            helper,
        },
    );
    let log = dirs._tmp.path().join("helper.log");
    (state, dirs, log)
}

fn post(uri: &str, body: Value) -> Request<String> {
    Request::builder()
        .method("POST")
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
        .body(body.to_string())
        .unwrap()
}

#[tokio::test]
async fn enroll_binds_pubkey_and_installs_peer() {
    let (state, dirs, log) = enroll_state();
    let app = papin_gateway::server::router(state);

    let resp = app
        .clone()
        .oneshot(post("/api/v1/enroll", json!({"client_pubkey": PUBKEY_A})))
        .await
        .unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    assert_eq!(
        status,
        StatusCode::OK,
        "body: {}",
        String::from_utf8_lossy(&bytes)
    );
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["assigned_ip"], "10.77.0.2");
    assert_eq!(body["gateway_ip"], "10.77.0.1");

    // The helper was invoked as `apply <peers-file>`.
    let log_text = std::fs::read_to_string(&log).unwrap();
    assert!(log_text.contains("apply "));

    // The peers file passes the real helper's validator.
    let peers = std::fs::read_dir(dirs._tmp.path().join("wg-peers"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let contents = std::fs::read_to_string(&peers).unwrap();
    assert_eq!(contents, "[Peer]\nPublicKey = AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8=\nAllowedIPs = 10.77.0.2/32\n");
    assert!(papin_gateway::validate_peers(&contents).is_ok());

    // Re-enrollment (re-keying) overwrites the pubkey and keeps the IP.
    let resp = app
        .oneshot(post("/api/v1/enroll", json!({"client_pubkey": PUBKEY_B})))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["assigned_ip"], "10.77.0.2");
    let contents = std::fs::read_to_string(&peers).unwrap();
    assert!(contents.contains(PUBKEY_B));
}

#[tokio::test]
async fn enroll_validation_errors() {
    let (state, _dirs, _log) = enroll_state();
    let app = papin_gateway::server::router(state);

    // Bad pubkey → 400.
    let resp = app
        .clone()
        .oneshot(post(
            "/api/v1/enroll",
            json!({"client_pubkey": "not-base64!!"}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    // Missing field → 400.
    let resp = app
        .clone()
        .oneshot(post("/api/v1/enroll", json!({})))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    // No token → 401.
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/enroll")
                .header(header::CONTENT_TYPE, "application/json")
                .body(json!({"client_pubkey": PUBKEY_A}).to_string())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn status_reports_versions_agents_and_wireguard() {
    let (state, _dirs, _log) = enroll_state();
    let app = papin_gateway::server::router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/v1/status")
                .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
                .body(String::new())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["protocol"], 1);
    assert!(body["version"].as_str().is_some());
    assert_eq!(body["agents"]["total"], 0);
    assert_eq!(body["agents"]["active"], 0);
    assert!(["up", "down", "unknown", "present"]
        .contains(&body["wireguard"]["state"].as_str().unwrap()));
}

#[test]
fn ip_allocation_skips_gateway_and_used() {
    use papin_gateway::config::{TokenEntry, TokenStore};
    let now = papin_gateway::config::now_secs();
    let store = TokenStore::from_entries(
        vec![TokenEntry {
            token: "t1".into(),
            created_at: now,
            expires_at: Some(now + 100),
            assigned_ip: Some("10.77.0.2".into()),
            pubkey: None,
            name: None,
        }],
        std::time::Duration::from_secs(100),
    );
    // 10.77.0.1 is the gateway, .2 is used → next is .3.
    assert_eq!(
        store
            .allocate_ip("10.77.0.0/24", "10.77.0.1")
            .map(|ip| ip.to_string()),
        Some("10.77.0.3".to_string())
    );
    // Exhausted pool (.1 gateway, .2-.254 used in a /24).
    let store2 = TokenStore::from_entries(
        (1..=254)
            .map(|i| TokenEntry {
                token: format!("t{i}"),
                created_at: now,
                expires_at: Some(now + 100),
                assigned_ip: Some(format!("10.77.0.{i}")),
                pubkey: None,
                name: None,
            })
            .collect(),
        std::time::Duration::from_secs(100),
    );
    assert!(store2.allocate_ip("10.77.0.0/24", "10.77.0.1").is_none());
    // IPv6 pools work too.
    let store3 = TokenStore::default_entries();
    assert_eq!(
        store3
            .allocate_ip("fd00::/64", "fd00::1")
            .map(|ip| ip.to_string()),
        Some("fd00::2".to_string())
    );
}
