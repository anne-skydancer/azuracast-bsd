mod annotate;
mod audio_processing;
mod autodj;
mod callbacks;
mod config;
mod control;
mod crossfade;
mod decode;
mod feedback;
mod harbor;
mod hls;
mod media;
mod output;
mod pipeline;
mod prepare;
mod queue;
mod server;

use std::io::Read;
use std::sync::Arc;

use config::EngineConfig;

const VERSION: &str = "0.1.0";

fn main() {
    let args: Vec<String> = std::env::args().collect();

    if args.iter().any(|a| a == "--version") {
        println!("azuracast-engine {VERSION}");
        std::process::exit(0);
    }

    if let Some(pos) = args.iter().position(|a| a == "--crossfade-test") {
        let file1 = args.get(pos + 1).cloned();
        let file2 = args.get(pos + 2).cloned();
        let (file1, file2) = match (file1, file2) {
            (Some(a), Some(b)) => (a, b),
            _ => {
                eprintln!(
                    "usage: azuracast-engine --crossfade-test <file1> <file2> --duration <seconds> --mode <smart|normal|disabled> --output <output.wav>"
                );
                std::process::exit(1);
            }
        };
        let duration: f64 = flag_value(&args, "--duration")
            .and_then(|s| s.parse().ok())
            .unwrap_or(crossfade::DEFAULT_FADE_SECONDS);
        let mode_str = flag_value(&args, "--mode").unwrap_or_else(|| "smart".to_string());
        let output_path =
            flag_value(&args, "--output").unwrap_or_else(|| "crossfade-test-output.wav".to_string());

        init_logging();
        match run_crossfade_test(&file1, &file2, duration, &mode_str, &output_path) {
            Ok(()) => {
                println!("wrote {output_path}");
                std::process::exit(0);
            }
            Err(e) => {
                eprintln!("error: {e}");
                std::process::exit(1);
            }
        }
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

    let queues = Arc::new(queue::TrackQueues::new());
    // Shared skip/metadata control-plane state between the axum handlers
    // (`server.rs`) and the pipeline loop (`pipeline.rs`) -- see
    // `control.rs`'s module doc for why this is a non-blocking poll rather
    // than a wait.
    let control = Arc::new(control::ControlSignals::new());
    // Phase 4: shared live-DJ harbor state (`harbor.rs`) -- written by the
    // harbor listener's connection-handling tasks, read by the pipeline
    // loop and `/streamer/disconnect`.
    let live = Arc::new(harbor::LiveState::new());

    let state = server::AppState {
        control_api_key: cfg.control_api.api_key.clone(),
        queues: queues.clone(),
        control: control.clone(),
        live: live.clone(),
    };

    // `bind_address` is a bare IP literal (no brackets, no port) per the
    // config contract — e.g. "127.0.0.1", "::1", or
    // "2001:8a0:6a32:2100::110". Parsing it as `std::net::IpAddr` (which
    // correctly handles both IPv4 and unbracketed IPv6 forms) and building a
    // `SocketAddr` from the parsed IP + port avoids the naive
    // `format!("{ip}:{port}")` string-concatenation bug: for IPv6, that
    // would produce an ambiguous/unparseable string like
    // "2001:8a0:6a32:2100::110:5000" because the address itself already
    // contains colons and must be bracketed before combining with a port in
    // host:port notation.
    let ip_addr: std::net::IpAddr = match cfg.control_api.bind_address.parse() {
        Ok(ip) => ip,
        Err(e) => {
            tracing::error!(
                "invalid control_api.bind_address '{}': not a valid IPv4 or IPv6 address: {e}",
                cfg.control_api.bind_address
            );
            std::process::exit(1);
        }
    };
    let bind_addr = std::net::SocketAddr::new(ip_addr, cfg.control_api.port);
    let listener = match tokio::net::TcpListener::bind(bind_addr).await {
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

    // Phase 4: the live-DJ harbor TCP listener (no-op/doesn't bind at all
    // if `harbor.enabled = false` -- see `harbor.rs`'s module doc).
    let harbor_task = tokio::spawn(harbor::run_harbor_listener(
        cfg.harbor.clone(),
        callback_client.clone(),
        live.clone(),
    ));

    // Phase 5: fan-out tap the pipeline broadcasts every produced PCM chunk
    // to (see `pipeline.rs`'s `OutputSink`), independent of its local file
    // sink. Capacity is generous slack for a handful of independent,
    // possibly briefly-lagging output tasks; a lagging receiver just skips
    // forward to newer audio (see `output.rs::run_output_once`) rather than
    // blocking the pipeline or any other output target.
    let (audio_tap_tx, _) = tokio::sync::broadcast::channel::<std::sync::Arc<Vec<f32>>>(64);

    // Phase 5: one independent encode+push task per configured local mount
    // and per configured remote relay -- see `output.rs`'s module doc for
    // the full scope (Icecast2 source-client protocol only, no
    // `share_encoders`, no HLS, no legacy Shoutcast remotes). No-op (spawns
    // nothing) if neither `[icecast_output]`/`[[mounts]]` nor `[[remotes]]`
    // are configured.
    let output_targets = output::build_targets(&cfg);
    if output_targets.is_empty() {
        tracing::info!("no Icecast/relay output targets configured; network output disabled");
    }
    for target in output_targets {
        let tap_rx_source = audio_tap_tx.clone();
        tokio::spawn(async move {
            output::run_output_target(target, tap_rx_source).await;
        });
    }

    // HLS output (SPEC.md B.8, deferred from Phase 5, now implemented) --
    // one independent ffmpeg segmenter task per configured rendition, same
    // broadcast-tap pattern as the Icecast/relay targets above. File-based,
    // not a network target -- see `hls.rs`'s module doc. No-op (spawns
    // nothing) if the station has no `[hls]` section or `[[hls_streams]]`
    // entries configured.
    if let Some(hls_cfg) = &cfg.hls {
        if cfg.hls_streams.is_empty() {
            tracing::warn!("[hls] section present but no [[hls_streams]] configured; HLS output disabled");
        } else {
            if let Err(e) = hls::write_master_playlist(hls_cfg, &cfg.hls_streams) {
                tracing::error!("failed to write HLS master playlist: {e}");
            }

            for stream in cfg.hls_streams.clone() {
                let hls_cfg = hls_cfg.clone();
                let tap_rx_source = audio_tap_tx.clone();
                tokio::spawn(async move {
                    hls::run_hls_rendition(stream, hls_cfg, tap_rx_source).await;
                });
            }
        }
    }

    // Phase 3: the real decode/crossfade/AutoDJ playback pipeline, replacing
    // Phase 2's demo `nextsong_loop`. See `pipeline.rs` for the full
    // orchestration (priority queues -> live harbor -> AutoDJ -> decode ->
    // autocue/replaygain -> crossfade -> feedback -> local file sink +
    // Phase 5's network output tap).
    let pipeline = pipeline::Pipeline::new(
        callback_client.clone(),
        queues.clone(),
        control.clone(),
        live.clone(),
        &cfg,
        audio_tap_tx,
    );
    let pipeline_task = tokio::spawn(async move { pipeline.run().await });

    tokio::select! {
        res = server_task => {
            match res {
                Ok(Ok(())) => tracing::warn!("control API server exited"),
                Ok(Err(e)) => tracing::error!("control API server error: {e}"),
                Err(e) => tracing::error!("control API server task panicked: {e}"),
            }
        }
        res = harbor_task => {
            match res {
                Ok(()) => tracing::warn!("harbor listener task exited unexpectedly"),
                Err(e) => tracing::error!("harbor listener task panicked: {e}"),
            }
        }
        res = pipeline_task => {
            match res {
                Ok(()) => tracing::warn!("playback pipeline exited unexpectedly"),
                Err(e) => tracing::error!("playback pipeline task panicked: {e}"),
            }
        }
        () = shutdown_signal() => {
            tracing::info!("shutdown signal received");
        }
    }

    tracing::info!("shutting down");
}

/// Returns the value immediately following `flag` in `args`, if present
/// (e.g. `flag_value(&args, "--duration")` for `... --duration 3.5 ...`).
fn flag_value(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|pos| args.get(pos + 1))
        .cloned()
}

/// `--crossfade-test` CLI mode: decodes two local files, runs them through
/// the crossfade logic with the given mode/duration, and writes the
/// resulting mixed PCM to a WAV file via `hound` -- a concrete, listenable
/// A/B test for the crossfade math, independent of the control API,
/// callbacks, or any network access.
fn run_crossfade_test(
    file1: &str,
    file2: &str,
    duration: f64,
    mode_str: &str,
    output_path: &str,
) -> Result<(), String> {
    let mode = match mode_str {
        "smart" => crossfade::CrossfadeMode::Smart,
        "normal" => crossfade::CrossfadeMode::Normal,
        "disabled" => crossfade::CrossfadeMode::Disabled,
        other => {
            return Err(format!(
                "unknown --mode '{other}' (expected smart|normal|disabled)"
            ))
        }
    };

    tracing::info!("decoding {file1}");
    let track_a = decode::decode_to_pcm(std::path::Path::new(file1))?;
    tracing::info!("decoding {file2}");
    let track_b = decode::decode_to_pcm(std::path::Path::new(file2))?;

    let channels = decode::PIPELINE_CHANNELS as usize;
    let window_frames = (duration * decode::PIPELINE_SAMPLE_RATE as f64).round() as usize;

    let a_frames = track_a.frames();
    let body_end = a_frames.saturating_sub(window_frames);
    let old_tail = &track_a.samples[body_end * channels..];

    let head_frames = window_frames.min(track_b.frames());
    let new_head = &track_b.samples[..head_frames * channels];

    let params = crossfade::CrossfadeParams {
        mode,
        fade_in_secs: duration,
        fade_out_secs: duration,
        thresholds: crossfade::CrossfadeThresholds::default(),
        to_live: false,
    };

    tracing::info!("mixing transition (mode={mode_str}, duration={duration}s)");
    let mixed = crossfade::mix_transition(
        old_tail,
        new_head,
        decode::PIPELINE_SAMPLE_RATE,
        &params,
    );

    // Full output: track A's un-touched body, the mixed transition, then
    // whatever remains of track B after its consumed head.
    let mut all_samples =
        Vec::with_capacity(body_end * channels + mixed.len() + track_b.samples.len());
    all_samples.extend_from_slice(&track_a.samples[..body_end * channels]);
    all_samples.extend_from_slice(&mixed);
    all_samples.extend_from_slice(&track_b.samples[head_frames * channels..]);

    write_wav(
        output_path,
        &all_samples,
        decode::PIPELINE_SAMPLE_RATE,
        decode::PIPELINE_CHANNELS,
    )
}

fn write_wav(path: &str, samples: &[f32], sample_rate: u32, channels: u16) -> Result<(), String> {
    let spec = hound::WavSpec {
        channels,
        sample_rate,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let mut writer = hound::WavWriter::create(path, spec)
        .map_err(|e| format!("failed to create {path}: {e}"))?;
    for s in samples {
        writer
            .write_sample(*s)
            .map_err(|e| format!("failed writing sample to {path}: {e}"))?;
    }
    writer
        .finalize()
        .map_err(|e| format!("failed finalizing {path}: {e}"))?;
    Ok(())
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
