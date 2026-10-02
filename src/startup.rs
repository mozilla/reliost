use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::Arc;

use actix_cors::Cors;
use actix_web::{dev::Server, web, App, HttpServer};
use samply_quota_manager::QuotaManager;
use tracing_actix_web::TracingLogger;

use crate::configuration::Settings;
use crate::proguard::MappingFileStore;
use crate::routes::{
    asm_v1, deobfuscate_java_v1, greet, heartbeat, lbheartbeat, self_profiles_index,
    self_profiles_latest, symbolicate_v5, version,
};
use crate::symbol_manager::{create_quota_manager, create_symbol_manager};

pub fn run(
    listener: TcpListener,
    settings: Settings,
) -> Result<(Server, Option<QuotaManager>), std::io::Error> {
    let workers = settings.server.workers;
    let self_profiles_dir: web::Data<Option<PathBuf>> =
        web::Data::new(settings.self_profiles.as_ref().map(|s| s.dir.clone()));
    let quota_manager = create_quota_manager(&settings);
    let quota_manager_notifiers: Vec<_> = quota_manager.iter().map(|qm| qm.notifier()).collect();
    let symbol_manager = create_symbol_manager(&settings, quota_manager_notifiers.clone());
    let app_data = web::Data::new(Arc::new(symbol_manager));
    let mapping_file_store = web::Data::new(Arc::new(MappingFileStore::new(
        settings.proguard,
        quota_manager_notifiers,
    )));
    let mut server = HttpServer::new(move || {
        let cors = Cors::default()
            .allow_any_origin()
            .allowed_methods(vec!["GET", "POST", "OPTION"])
            .allow_any_header()
            .send_wildcard()
            .max_age(86400);
        App::new()
            .wrap(cors)
            .wrap(TracingLogger::default())
            .route("/", web::get().to(greet))
            .route("/symbolicate/v5", web::post().to(symbolicate_v5))
            .route("/asm/v1", web::post().to(asm_v1))
            .route("/deobfuscate/java/v1", web::post().to(deobfuscate_java_v1))
            .route("/self-profiles/", web::get().to(self_profiles_index))
            .route(
                "/self-profiles/latest.json.gz",
                web::get().to(self_profiles_latest),
            )
            // Dockerflow requirements. See:
            // https://github.com/mozilla-services/Dockerflow#containerized-app-requirements
            .route("/__version__", web::get().to(version))
            .route("/__heartbeat__", web::get().to(heartbeat))
            .route("/__lbheartbeat__", web::get().to(lbheartbeat))
            .app_data(app_data.clone())
            .app_data(mapping_file_store.clone())
            .app_data(self_profiles_dir.clone())
            .app_data(web::PayloadConfig::new(100 * 1000 * 1000)) // 100 MB
    });
    if let Some(workers) = workers {
        server = server.workers(workers);
    }
    let server = server.listen(listener)?.run();
    Ok((server, quota_manager))
}
