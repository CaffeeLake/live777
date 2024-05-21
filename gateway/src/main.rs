use crate::route::r#static::static_server;
use axum::body::Body;
use axum::extract::Request;
use axum::Router;
use clap::Parser;

use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::rt::TokioExecutor;
use sqlx::mysql::MySqlConnectOptions;
use sqlx::MySqlPool;
use std::future::IntoFuture;
use std::str::FromStr;

use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;
use tower_http::validate_request::ValidateRequestHeaderLayer;
use tracing::{debug, error, info, info_span, warn};

use crate::auth::ManyValidate;
use crate::config::Config;

mod auth;
mod config;
mod db;
mod error;
mod model;
mod result;
mod route;
mod tick;

#[derive(Parser)]
#[command(version)]
struct Args {
    /// Set config file path
    #[arg(short, long)]
    config: Option<String>,
}

#[tokio::main]
async fn main() {
    sqlx::any::install_default_drivers();
    let args = Args::parse();
    let cfg = Config::parse(args.config);
    utils::set_log(format!(
        "live777_gateway={},live777_storage={},sqlx={},webrtc=error",
        cfg.log.level, cfg.log.level, cfg.log.level
    ));
    warn!("set log level : {}", cfg.log.level);
    debug!("config : {:?}", cfg);
    let listener = tokio::net::TcpListener::bind(cfg.http.listen)
        .await
        .unwrap();
    info!("Server listening on {}", listener.local_addr().unwrap());
    let pool_connect_options = MySqlConnectOptions::from_str(&cfg.db_url).unwrap();
    let client: Client =
        hyper_util::client::legacy::Client::<(), ()>::builder(TokioExecutor::new())
            .build(HttpConnector::new());
    let app_state = AppState {
        config: cfg.clone(),
        pool: MySqlPool::connect_with(pool_connect_options)
            .await
            .map_err(|e| anyhow::anyhow!(format!("MySQL error : {}", e)))
            .unwrap(),
        client,
    };
    let auth_layer = ValidateRequestHeaderLayer::custom(ManyValidate::new(vec![cfg.auth]));
    let manager_auth_layer =
        ValidateRequestHeaderLayer::custom(ManyValidate::new(vec![cfg.manager_auth]));
    let app = Router::new()
        .merge(route::proxy::route().layer(auth_layer))
        .merge(route::manager::route().layer(manager_auth_layer))
        .merge(route::hook::route())
        .with_state(app_state.clone())
        .layer(if cfg.http.cors {
            CorsLayer::permissive()
        } else {
            CorsLayer::new()
        })
        .layer(axum::middleware::from_fn(http_utils::print_request_response))
        .layer(
            TraceLayer::new_for_http().make_span_with(|request: &Request<_>| {
                let span = info_span!(
                    "http_request",
                    uri = ?request.uri(),
                    method = ?request.method(),
                    span_id = tracing::field::Empty,
                    target_addr = tracing::field::Empty,
                );
                span.record("span_id", span.id().unwrap().into_u64());
                span
            }),
        );
    tokio::spawn(tick::run(app_state));
    tokio::select! {
        Err(e) = axum::serve(listener, static_server(app)).into_future() => error!("Application error: {e}"),
        msg = signal::wait_for_stop_signal() => debug!("Received signal: {}", msg),
    }
    info!("Server shutdown");
}

type Client = hyper_util::client::legacy::Client<HttpConnector, Body>;

#[derive(Clone)]
struct AppState {
    config: Config,
    pool: MySqlPool,
    client: Client,
}
