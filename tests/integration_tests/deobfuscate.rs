use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use actix_web::{web, App, HttpResponse, HttpServer};
use reliost::configuration::ProguardSettings;
use serde_json::{json, Value};

const UUID: &str = "fe506e08-58e3-3f15-9117-67ccb4d01f19";

const MAPPING: &str = r#"# compiler: R8
org.example.Outer -> a.a:
# {"id":"sourceFile","fileName":"Outer.kt"}
    1:5:void outer():469:469 -> a
    6:10:double org.example.Inlined.inner():21:21 -> a
    6:10:void outer():470 -> a
org.example.SomeException -> a.b:
# {"id":"sourceFile","fileName":"SomeException.kt"}
"#;

/// A cache directory which is deleted when the test ends.
struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "reliost-test-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        Self(path)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Serves `MAPPING` at `/<UUID>/mapping.txt` and returns the
/// server's base URL and a counter of requests for that file.
fn spawn_mapping_server() -> (String, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("Failed to bind random port");
    let port = listener.local_addr().unwrap().port();
    let hits = Arc::new(AtomicUsize::new(0));
    let hits_for_server = hits.clone();
    let server = HttpServer::new(move || {
        let hits = hits_for_server.clone();
        App::new().route(
            &format!("/{UUID}/mapping.txt"),
            web::get().to(move || {
                hits.fetch_add(1, Ordering::SeqCst);
                async { HttpResponse::Ok().body(MAPPING) }
            }),
        )
    })
    .workers(1)
    .listen(listener)
    .unwrap()
    .run();
    tokio::spawn(server);
    (format!("http://127.0.0.1:{port}"), hits)
}

async fn post(address: &str, body: &str) -> reqwest::Response {
    reqwest::Client::new()
        .post(format!("http://{address}/deobfuscate/java/v1"))
        .header("Content-Type", "application/json")
        .body(body.to_owned())
        .send()
        .await
        .expect("Failed to execute request.")
}

fn request(uuid: &str) -> String {
    json!({
        "jobs": [{
            "mappingFile": { "uuid": uuid },
            "stacks": [{
                "exception": "a.b",
                "frames": [
                    { "class": "a.a", "method": "a", "line": 7, "file": "SourceFile" },
                    { "class": "a.a", "method": "a", "line": 2 }
                ]
            }],
            "classes": ["a.b", "x.y"]
        }]
    })
    .to_string()
}

#[tokio::test]
async fn deobfuscate_java_v1_remaps_frames_and_caches_mapping() {
    let (server_url, hits) = spawn_mapping_server();
    let cache_dir = TempDir::new("deobfuscate");
    let (address, _join_handle) = super::spawn_app_with_proguard_settings(Some(ProguardSettings {
        servers: vec![server_url],
        cache_dir: cache_dir.0.clone(),
    }));

    let expected = json!({
        "results": [{
            "mappingFile": { "found": true },
            "stacks": [{
                "exception": "org.example.SomeException",
                "frames": [
                    [
                        { "class": "org.example.Inlined", "method": "inner", "file": "Inlined.kt", "line": 21 },
                        { "class": "org.example.Outer", "method": "outer", "file": "Outer.kt", "line": 470 }
                    ],
                    [
                        { "class": "org.example.Outer", "method": "outer", "file": "Outer.kt", "line": 469 }
                    ]
                ]
            }],
            "classes": { "a.b": "org.example.SomeException" }
        }]
    });

    for _ in 0..2 {
        let response = post(&address, &request(UUID)).await;
        assert_eq!(response.status(), 200);
        let body: Value = response.json().await.expect("Response was not valid JSON");
        assert_eq!(body, expected);
    }

    // The second request is answered from the cache.
    assert_eq!(hits.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn deobfuscate_java_v1_concurrent_requests_download_once() {
    let (server_url, hits) = spawn_mapping_server();
    let cache_dir = TempDir::new("deobfuscate-concurrent");
    let (address, _join_handle) = super::spawn_app_with_proguard_settings(Some(ProguardSettings {
        servers: vec![server_url],
        cache_dir: cache_dir.0.clone(),
    }));

    let body = request(UUID);
    let responses = tokio::join!(
        post(&address, &body),
        post(&address, &body),
        post(&address, &body)
    );
    for response in [responses.0, responses.1, responses.2] {
        let body: Value = response.json().await.unwrap();
        assert_eq!(body["results"][0]["mappingFile"]["found"], true);
    }
    assert_eq!(hits.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn deobfuscate_java_v1_missing_mapping_returns_frames_unchanged() {
    let (server_url, _hits) = spawn_mapping_server();
    let cache_dir = TempDir::new("deobfuscate-missing");
    let (address, _join_handle) = super::spawn_app_with_proguard_settings(Some(ProguardSettings {
        servers: vec![server_url],
        cache_dir: cache_dir.0.clone(),
    }));

    let response = post(&address, &request("00000000-0000-0000-0000-000000000000")).await;
    assert_eq!(response.status(), 200);
    let body: Value = response.json().await.unwrap();
    let result = &body["results"][0];
    assert_eq!(result["mappingFile"]["found"], false);
    assert!(result["mappingFile"]["error"]
        .as_str()
        .unwrap()
        .contains("not found"));
    assert_eq!(result["stacks"][0]["exception"], "a.b");
    assert_eq!(
        result["stacks"][0]["frames"][0],
        json!([{ "class": "a.a", "method": "a", "line": 7, "file": "SourceFile" }])
    );
    assert_eq!(result["classes"], json!({}));
}

#[tokio::test]
async fn deobfuscate_java_v1_rejects_invalid_uuid() {
    let (address, _join_handle) = super::spawn_app();

    let body = json!({
        "jobs": [{ "mappingFile": { "uuid": "../etc" }, "stacks": [] }]
    });
    let response = post(&address, &body.to_string()).await;
    assert_eq!(response.status(), 200);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["results"][0]["mappingFile"]["found"], false);
    assert!(body["results"][0]["mappingFile"]["error"]
        .as_str()
        .unwrap()
        .contains("Invalid ProGuard UUID"));
}

#[tokio::test]
async fn deobfuscate_java_v1_rejects_malformed_request() {
    let (address, _join_handle) = super::spawn_app();

    let response = post(&address, r#"{"jobs": [{"stacks": []}]}"#).await;
    assert_eq!(response.status(), 400);
    let body: Value = response.json().await.unwrap();
    assert!(body["error"].as_str().unwrap().contains("mappingFile"));
}
