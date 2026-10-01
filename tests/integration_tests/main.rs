mod asm;
mod dockerflow;
mod routing;
mod symbolicate;

use std::net::TcpListener;

use reliost::{configuration::ServerSettings, configuration::Settings};
use tokio::task::JoinHandle;

fn spawn_app() -> (String, JoinHandle<Result<(), std::io::Error>>) {
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
        proguard: None,
        quota: None,
        self_profiles: None,
    };
    let (server, _) = reliost::startup::run(listener, settings).expect("Failed to bind address.");
    let join_handle = tokio::spawn(server);
    (format!("{host}:{port}"), join_handle)
}
