mod asm;
mod deobfuscate;
mod dockerflow;
mod routing;
mod symbolicate;

use std::net::TcpListener;

use reliost::configuration::{ProguardSettings, ServerSettings, Settings};
use tokio::task::JoinHandle;

fn spawn_app() -> (String, JoinHandle<Result<(), std::io::Error>>) {
    spawn_app_with_proguard_settings(None)
}

fn spawn_app_with_proguard_settings(
    proguard: Option<ProguardSettings>,
) -> (String, JoinHandle<Result<(), std::io::Error>>) {
    let host = "127.0.0.1";
    let listener = TcpListener::bind(format!("{host}:0")).expect("Failed to bind random port");
    let port = listener.local_addr().unwrap().port();
    let settings = Settings {
        server: ServerSettings {
            host: host.to_string(),
            port,
            workers: None,
        },
        symbols: None,
        proguard,
        quota: None,
        self_profiles: None,
    };
    let (server, _) = reliost::startup::run(listener, settings).expect("Failed to bind address.");
    let join_handle = tokio::spawn(server);
    (format!("{host}:{port}"), join_handle)
}
