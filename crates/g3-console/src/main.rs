mod api;
mod logs;
mod models;
mod process;
mod launch;

use api::control::{kill_instance, launch_instance, restart_instance, ControllerState};
use api::instances::{get_instance, list_instances, AppState};
use api::logs::get_instance_logs;
use api::state::{get_state, save_state, browse_filesystem};
use axum::{
    routing::{get, post},
    Router,
};
use clap::Parser;
use process::{ProcessController, ProcessDetector};
use std::sync::Arc;
use tokio::sync::Mutex;
use tower_http::cors::CorsLayer;
use tower_http::services::ServeDir;
use tracing::{info, Level};
use tracing_subscriber;

#[derive(Parser, Debug)]
#[command(name = "g3-console")]
#[command(about = "Web console for monitoring and managing g3 instances")]
struct Args {
    /// Port to bind to
    #[arg(long, default_value = "9090")]
    port: u16,

    /// Host to bind to
    #[arg(long, default_value = "127.0.0.1")]
    host: String,

    /// Auto-open browser
    #[arg(long)]
    open: bool,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_max_level(Level::INFO)
        .init();

    let args = Args::parse();

    // Create shared state
    let detector = Arc::new(Mutex::new(ProcessDetector::new()));
    let controller = Arc::new(Mutex::new(ProcessController::new()));

    // Build API routes with different state for different endpoints
    let instance_routes = Router::new()
        .route("/instances", get(list_instances))
        .route("/instances/:id", get(get_instance))
        .route("/instances/:id/logs", get(get_instance_logs))
        .with_state(detector.clone());

    let control_routes = Router::new()
        .route("/instances/:id/kill", post(kill_instance))
        .route("/instances/:id/restart", post(restart_instance))
        .route("/instances/launch", post(launch_instance))
        .with_state(controller.clone());

    let state_routes = Router::new()
        .route("/state", get(get_state))
        .route("/state", post(save_state))
        .route("/browse", post(browse_filesystem))
        .with_state(controller.clone());

    // Combine routes
    let api_routes = Router::new()
        .merge(instance_routes)
        .merge(control_routes)
        .merge(state_routes);

    // Serve static files from web directory
    let web_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("web");
    let static_service = ServeDir::new(web_dir);

    // Build main app
    let app = Router::new()
        .nest("/api", api_routes)
        .fallback_service(static_service)
        .layer(CorsLayer::permissive());

    let addr = format!("{}:{}", args.host, args.port);
    info!("Starting g3-console on http://{}", addr);

    // Auto-open browser if requested
    if args.open {
        let url = format!("http://{}", addr);
        info!("Opening browser to {}", url);
        let _ = open::that(&url);
    }

    // Start server
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}
