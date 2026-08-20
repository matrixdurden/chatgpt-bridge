mod auth;
mod cli;
mod config;
mod error;
mod routes;
mod updater;
mod workspace;

use anyhow::{Context, Result};
use axum::{
    Router,
    extract::Request,
    http::{HeaderValue, Method, header::CONTENT_TYPE},
    middleware::{self, Next},
    response::Response,
    routing::{get, post},
};
use axum_server::tls_rustls::RustlsConfig;
use cli::CliCommand;
use config::Config;
use ngrok::{config::ForwarderBuilder, tunnel::EndpointInfo};
use std::{env, sync::Arc};
use tokio::net::TcpListener;
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;
use url::Url;

const PUBLIC_URL_FILE: &str = "/run/chatgpt-bridge/public-url";

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = env::args().skip(1).collect::<Vec<_>>();
    if args.first().is_some_and(|arg| arg == "update") {
        return updater::run_args(&args[1..]);
    }

    let command = cli::parse_args(args)?;
    if command == CliCommand::Help {
        cli::execute(command)?;
        updater::print_main_help_extension();
        return Ok(());
    }
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
        .route_layer(middleware::from_fn(restore_missing_json_content_type))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            auth::require_bearer,
        ));

    let app = Router::new()
        .route("/health", get(routes::health))
        .merge(protected)
        .with_state(state);

    if config.ngrok_enabled {
        serve_with_ngrok(bind, app, &config).await?;
    } else if let Some(tls) = &config.tls {
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

async fn serve_with_ngrok(bind: std::net::SocketAddr, app: Router, config: &Config) -> Result<()> {
    let listener = TcpListener::bind(bind).await?;
    let upstream = Url::parse(&format!("http://{bind}"))
        .context("failed to construct local ngrok upstream URL")?;

    let session = ngrok::Session::builder()
        .authtoken_from_env()
        .connect()
        .await
        .context("failed to connect to ngrok; check the saved ngrok authtoken")?;

    let tunnel = session
        .http_endpoint()
        .listen_and_forward(upstream)
        .await
        .context("failed to create ngrok public endpoint")?;

    let public_url = tunnel.url().to_owned();
    if let Err(error) = tokio::fs::write(PUBLIC_URL_FILE, &public_url).await {
        warn!(%error, "failed to write public URL state file");
    }

    info!(
        %bind,
        %public_url,
        workspace = %config.root.display(),
        version = env!("CARGO_PKG_VERSION"),
        "chatgpt-bridge public ngrok endpoint started"
    );

    let result = axum::serve(listener, app).await;
    drop(tunnel);
    result?;
    Ok(())
}

async fn restore_missing_json_content_type(mut request: Request, next: Next) -> Response {
    // Some reverse proxies have stripped Content-Type from proxied POST requests.
    // All protected POST endpoints in this bridge are JSON-only, so restoring
    // the header when it is missing is unambiguous. Never overwrite a value
    // explicitly supplied by the caller.
    if request.method() == Method::POST && !request.headers().contains_key(CONTENT_TYPE) {
        request
            .headers_mut()
            .insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    }

    next.run(request).await
}
