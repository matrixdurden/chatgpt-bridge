mod auth;
mod cli;
mod config;
mod error;
mod routes;
mod workspace;

use anyhow::Result;
use axum::{
    Router, middleware,
    routing::{get, post},
};
use axum_server::tls_rustls::RustlsConfig;
use cli::CliCommand;
use config::Config;
use std::{env, sync::Arc};
use tokio::net::TcpListener;
use tracing::info;
use tracing_subscriber::EnvFilter;

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let command = cli::parse_args(env::args().skip(1))?;
    if command != CliCommand::Serve {
        return cli::execute(command);
    }

    serve().await
}

async fn serve() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("chatgpt_bridge=info")),
        )
        .init();

    let config = Arc::new(Config::from_env()?);
    let bind = config.bind;
    let state = AppState {
        config: Arc::clone(&config),
    };

    let protected = Router::new()
        .route("/v1/info", get(routes::info))
        .route("/v1/exec", post(routes::exec))
        .route("/v1/files/read", post(routes::read_file))
        .route("/v1/files/write", post(routes::write_file))
        .route("/v1/files/list", post(routes::list_files))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            auth::require_bearer,
        ));

    let app = Router::new()
        .route("/health", get(routes::health))
        .merge(protected)
        .with_state(state);

    if let Some(tls) = &config.tls {
        let tls_config = RustlsConfig::from_pem_file(&tls.cert, &tls.key).await?;
        info!(
            %bind,
            workspace = %config.root.display(),
            version = env!("CARGO_PKG_VERSION"),
            "chatgpt-bridge HTTPS server started"
        );

        axum_server::bind_rustls(bind, tls_config)
            .serve(app.into_make_service())
            .await?;
    } else {
        let listener = TcpListener::bind(bind).await?;
        info!(
            %bind,
            workspace = %config.root.display(),
            version = env!("CARGO_PKG_VERSION"),
            "chatgpt-bridge HTTP server started"
        );

        axum::serve(listener, app).await?;
    }

    Ok(())
}
