mod callbacks;
mod config;
mod server;

use std::io::Read;
use std::sync::Arc;
use std::time::Duration;

use config::EngineConfig;

const VERSION: &str = "0.1.0";

/// How often the nextsong polling loop calls the `nextsong` callback.
/// This is a Phase 2 "prove the wiring works" demo loop, not a real AutoDJ
/// scheduler (that's a later phase) — it doesn't act on the response.
const NEXTSONG_POLL_INTERVAL: Duration = Duration::from_secs(15);

fn main() {
    let args: Vec<String> = std::env::args().collect();

    if args.iter().any(|a| a == "--version") {
        println!("azuracast-engine {VERSION}");
        std::process::exit(0);
    }

    if let Some(pos) = args.iter().position(|a| a == "--check-config") {
        let target = args.get(pos + 1).map(|s| s.as_str());
        if target != Some("-") {
            eprintln!("--check-config requires '-' as its argument (config is read from stdin)");
            std::process::exit(1);
        }

        let mut buf = String::new();
        if let Err(e) = std::io::stdin().read_to_string(&mut buf) {
            eprintln!("error: failed to read config from stdin: {e}");
            std::process::exit(1);
        }

        match config::parse_config(&buf) {
            Ok(_) => std::process::exit(0),
            Err(e) => {
                eprintln!("error: invalid config: {e}");
                std::process::exit(1);
            }
        }
    }

    let config_path = match args.iter().position(|a| a == "--config") {
        Some(i) => match args.get(i + 1) {
            Some(p) => p.clone(),
            None => {
                eprintln!("error: --config requires a path argument");
                std::process::exit(1);
            }
        },
        None => {
            eprintln!(
                "usage: azuracast-engine --config <path-to-engine.toml>\n       azuracast-engine --check-config -\n       azuracast-engine --version"
            );
            std::process::exit(1);
        }
    };

    let cfg = match config::load_config(&config_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
    };

    // Args are fully handled synchronously above (per the spec: --version
    // and --check-config must not start a runtime or server at all); only
    // now do we spin up tokio for the actual foreground-server lifecycle.
    let rt = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("error: failed to create async runtime: {e}");
            std::process::exit(1);
        }
    };

    rt.block_on(run(cfg));
}

fn init_logging() {
    use tracing_subscriber::EnvFilter;

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .init();
}

async fn run(cfg: EngineConfig) {
    init_logging();

    tracing::info!(
        "azuracast-engine {VERSION} starting for station {} ({})",
        cfg.station.id,
        cfg.station.name
    );
    tracing::info!(
        "logging to stdout only in this phase; supervisord is expected to redirect stdout to {}",
        cfg.paths.log_file
    );

    let state = server::AppState {
        control_api_key: cfg.control_api.api_key.clone(),
    };

    let bind_addr = format!("{}:{}", cfg.control_api.bind_address, cfg.control_api.port);
    let listener = match tokio::net::TcpListener::bind(&bind_addr).await {
        Ok(l) => l,
        Err(e) => {
            tracing::error!("failed to bind control API to {bind_addr}: {e}");
            std::process::exit(1);
        }
    };
    tracing::info!("control API listening on {bind_addr}");

    let app = server::build_router(state);
    let callback_client = Arc::new(callbacks::CallbackClient::new(&cfg.callbacks));

    let server_task = tokio::spawn(async move { axum::serve(listener, app).await });

    let nextsong_client = callback_client.clone();
    let nextsong_task = tokio::spawn(async move {
        nextsong_loop(nextsong_client).await;
    });

    tokio::select! {
        res = server_task => {
            match res {
                Ok(Ok(())) => tracing::warn!("control API server exited"),
                Ok(Err(e)) => tracing::error!("control API server error: {e}"),
                Err(e) => tracing::error!("control API server task panicked: {e}"),
            }
        }
        res = nextsong_task => {
            match res {
                Ok(()) => tracing::warn!("nextsong polling loop exited unexpectedly"),
                Err(e) => tracing::error!("nextsong polling loop task panicked: {e}"),
            }
        }
        () = shutdown_signal() => {
            tracing::info!("shutdown signal received");
        }
    }

    tracing::info!("shutting down");
}

/// Demo loop proving the callback wiring works end-to-end in this phase:
/// periodically calls `nextsong` and logs the response (or the error, if
/// PHP isn't reachable). Does not act on the response — no real playback
/// exists yet.
async fn nextsong_loop(client: Arc<callbacks::CallbackClient>) {
    let mut interval = tokio::time::interval(NEXTSONG_POLL_INTERVAL);
    loop {
        interval.tick().await;
        match client.call_nextsong().await {
            Ok(resp) => tracing::info!("nextsong callback returned uri: {}", resp.uri),
            Err(e) => tracing::warn!("nextsong callback failed (continuing): {e}"),
        }
    }
}

/// Resolves once the process receives SIGTERM or SIGINT (Ctrl+C), so `run`
/// can shut down cleanly instead of daemonizing/forking — supervisord
/// expects to manage this process directly in the foreground.
async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        use tokio::signal::unix::{signal, SignalKind};
        signal(SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => {},
        () = terminate => {},
    }
}
