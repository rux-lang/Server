mod config;
mod health;

use std::sync::Arc;
use std::{net::SocketAddr, time::Duration};

use axum::Router;
use axum::http::StatusCode;
use axum::http::header::{AUTHORIZATION, COOKIE, SET_COOKIE};
use axum::middleware;
use config::ApiConfig;
use rux_application::{
    AccountLifecycleService, ApiTokenService, ApiTokens, Authentication, AuthenticationService,
    DashboardService, DependencyProbe, DiscoveryService, DownloadService, Downloads,
    NamespaceService, Namespaces, OrphanCleanupService, PackageMetadataService,
    PackageSearchService, PlaygroundExecution, PublicationService, PublicationWorkflow,
    Publications, ResolverIndexService, TokenAuthorizer, YankService, Yanks,
};
use rux_infrastructure::{GitHubOAuthClient, Infrastructure, OsCredentialGenerator, SystemClock};
use tokio::sync::watch;
use tower_http::request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer};
use tower_http::sensitive_headers::{
    SetSensitiveRequestHeadersLayer, SetSensitiveResponseHeadersLayer,
};
use tower_http::trace::TraceLayer;
use tracing::info;

struct Runtime {
    cleanup_interval: Duration,
    cleanup: Arc<OrphanCleanupService>,
    abuse: rux_server::abuse::AbuseControls,
}

enum Exit {
    Signal,
    Api(Result<std::io::Result<()>, tokio::task::JoinError>),
}

#[tokio::main]
#[allow(clippy::too_many_lines)]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = ApiConfig::from_environment()?;
    rux_server::logging::init()?;
    let cleanup_config = config.orphan_cleanup;
    let abuse = rux_server::abuse::AbuseControls::new(config.abuse.clone());
    let origin = config.allowed_web_origin.parse()?;
    let web_origin = config.allowed_web_origin.clone();
    let infrastructure = Arc::new(Infrastructure::new(config.infrastructure).await?);
    let probe: Arc<dyn DependencyProbe> = infrastructure.clone();
    let repository = Arc::new(infrastructure.repository().clone());
    let clock = Arc::new(SystemClock);
    let credentials = Arc::new(OsCredentialGenerator);
    let authentication: Arc<dyn Authentication> = Arc::new(AuthenticationService::new(
        repository.clone(),
        Arc::new(GitHubOAuthClient::new(config.github_oauth)?),
        clock.clone(),
        credentials.clone(),
    ));
    let token_service = Arc::new(ApiTokenService::new(
        repository.clone(),
        clock.clone(),
        credentials,
    ));
    let tokens: Arc<dyn ApiTokens> = token_service.clone();
    let token_authorizer: Arc<dyn TokenAuthorizer> = token_service;
    let namespaces: Arc<dyn Namespaces> = Arc::new(NamespaceService::new(
        repository.clone(),
        clock.clone(),
        token_authorizer.clone(),
    ));
    let dashboards = Arc::new(DashboardService::new(repository.clone(), clock.clone()));
    let resolver_indexes = Arc::new(ResolverIndexService::new(repository.clone()));
    let package_metadata = Arc::new(PackageMetadataService::new(repository.clone()));
    let package_search = Arc::new(PackageSearchService::new(repository.clone()));
    let discovery = Arc::new(DiscoveryService::new(repository.clone(), clock.clone()));
    let downloads: Arc<dyn Downloads> =
        Arc::new(DownloadService::new(repository.clone(), clock.clone()));
    let yanks: Arc<dyn Yanks> = Arc::new(YankService::new(
        repository.clone(),
        token_authorizer.clone(),
        clock.clone(),
    ));
    let publication_service = Arc::new(PublicationService::new(
        repository.clone(),
        token_authorizer,
    ));
    let artifact_storage = Arc::new(infrastructure.artifact_storage());
    let publications: Arc<dyn Publications> = Arc::new(PublicationWorkflow::new(
        publication_service,
        artifact_storage.clone(),
    ));
    let cleanup = Arc::new(OrphanCleanupService::new(
        artifact_storage,
        Arc::new(infrastructure.repository().clone()),
        clock.clone(),
        cleanup_config.policy,
    ));
    let uploads = rux_server::upload::UploadReceiver::new(
        &config.uploads.temporary_directory,
        config.uploads.temporary_capacity_bytes,
    )?;
    let health_routes = abuse.timeout_only(health::router(probe.clone()));
    let security_routes = abuse.protect(
        Router::new()
            .merge(rux_server::auth::router(
                authentication.clone(),
                config.web_callback_url,
                web_origin.clone(),
            ))
            .merge(rux_server::account::router(
                authentication.clone(),
                Arc::new(AccountLifecycleService::new(
                    repository.clone(),
                    clock.clone(),
                )),
                web_origin.clone(),
            ))
            .merge(rux_server::tokens::router(
                authentication.clone(),
                tokens,
                web_origin.clone(),
            )),
        rux_server::abuse::RateTier::Security,
    );
    let mutation_routes = abuse.protect(
        Router::new()
            .merge(rux_server::namespaces::router(
                authentication.clone(),
                namespaces,
                web_origin.clone(),
            ))
            .merge(rux_server::yanks::router(yanks)),
        rux_server::abuse::RateTier::Mutation,
    );
    let publication_routes =
        abuse.protect_publication(rux_server::publication::router(publications, uploads));
    let read_routes = abuse.protect(
        Router::new()
            .merge(rux_server::dashboard::router(
                authentication.clone(),
                dashboards,
                web_origin.clone(),
            ))
            .merge(rux_server::resolver::router(resolver_indexes))
            .merge(rux_server::metadata::router(package_metadata))
            .merge(rux_server::search::router(package_search))
            .merge(rux_server::discovery::router(discovery))
            .merge(rux_server::downloads::router(
                downloads,
                config.package_cdn_base_url,
            ))
            .merge(rux_server::openapi::router()),
        rux_server::abuse::RateTier::Read,
    );
    let fallback_routes = abuse.protect(
        Router::new().fallback(|| async {
            rux_server::contract::Problem::new(StatusCode::NOT_FOUND, "not_found", "Not Found")
        }),
        rux_server::abuse::RateTier::Read,
    );
    let mut app = Router::new()
        .merge(health_routes)
        .merge(security_routes)
        .merge(mutation_routes)
        .merge(publication_routes)
        .merge(read_routes);

    // Merged only when enabled, so a deployment without a sandbox answers 404
    // from the fallback rather than advertising an endpoint that always fails.
    if config.playground.enabled {
        let playground: Arc<dyn PlaygroundExecution> =
            Arc::new(rux_application::PlaygroundService::new(
                Arc::new(rux_infrastructure::UnixSocketPlayground::new(
                    config.playground.socket_path.clone(),
                    config.playground.timeout,
                )),
                // The daemon is authoritative for the limits it reports; this
                // is only the local guard that keeps an oversized submission
                // from occupying a socket, and it mirrors the same envelope.
                rux_application::PlaygroundLimits::default(),
            ));
        app = app.merge(abuse.protect_playground(rux_server::playground::router(
            playground,
            web_origin.clone(),
        )));
    }

    let app = app
        .merge(fallback_routes)
        .layer(middleware::from_fn_with_state(
            abuse.trusted_proxies(),
            rux_server::abuse::resolve_client_ip,
        ))
        .layer(SetSensitiveResponseHeadersLayer::new([SET_COOKIE]))
        .layer(rux_server::cors_layer(origin))
        .layer(PropagateRequestIdLayer::x_request_id())
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(rux_server::logging::request_span)
                .on_response(rux_server::logging::record_response),
        )
        .layer(SetSensitiveRequestHeadersLayer::new([
            AUTHORIZATION,
            COOKIE,
            http::HeaderName::from_static("x-csrf-token"),
        ]))
        .layer(SetRequestIdLayer::x_request_id(MakeRequestUuid))
        .layer(middleware::from_fn(
            rux_server::abuse::discard_supplied_request_id,
        ));
    serve(
        app,
        config.bind_address,
        Runtime {
            cleanup_interval: cleanup_config.interval,
            cleanup,
            abuse,
        },
    )
    .await
}

async fn serve(
    app: Router,
    bind_address: SocketAddr,
    runtime: Runtime,
) -> Result<(), Box<dyn std::error::Error>> {
    let listener = tokio::net::TcpListener::bind(bind_address).await?;
    let (shutdown_sender, shutdown_receiver) = watch::channel(false);
    let cleanup_task = tokio::spawn(rux_server::cleanup::run(
        runtime.cleanup,
        runtime.cleanup_interval,
        shutdown_receiver.clone(),
    ));
    let rate_limit_pruner = tokio::spawn(rux_server::abuse::run_pruner(
        runtime.abuse,
        shutdown_receiver.clone(),
    ));
    info!(address = %listener.local_addr()?, "Rux server API listening");

    let mut api_task = tokio::spawn(
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .with_graceful_shutdown(wait_for_shutdown(shutdown_receiver))
        .into_future(),
    );

    let exit = tokio::select! {
        _ = tokio::signal::ctrl_c() => Exit::Signal,
        result = &mut api_task => Exit::Api(result),
    };
    let _ = shutdown_sender.send(true);

    let server_result = match exit {
        Exit::Signal => {
            api_task.await??;
            Ok(())
        }
        Exit::Api(result) => {
            result??;
            Ok(())
        }
    };
    cleanup_task.await?;
    rate_limit_pruner.await?;
    server_result
}

async fn wait_for_shutdown(mut receiver: watch::Receiver<bool>) {
    while !*receiver.borrow() {
        if receiver.changed().await.is_err() {
            return;
        }
    }
}
