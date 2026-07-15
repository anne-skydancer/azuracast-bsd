//! HLS output (`engine/SPEC.md` B.8), deferred from Phase 5 and implemented
//! post-cutover. Unlike `output.rs`'s Icecast/relay targets, this is
//! **file-based, not a network protocol**: the historical Liquidsoap
//! integration segmented audio directly to disk (`output.file.hls(...)`),
//! and nginx serves that directory as-is (`Nginx\ConfigWriter::
//! writeHlsSection`, a PHP-side file this engine's cutover left completely
//! untouched, since it already just does `alias {hls_dir}; try_files $uri
//! =404;`). That means this module needs no new network-facing protocol
//! work at all -- it reuses `output.rs`'s established broadcast-tap +
//! ffmpeg-subprocess architecture, just pointed at ffmpeg's own `-f hls`
//! muxer instead of a `pipe:1`-then-TCP path.
//!
//! ## Scope
//!
//! - **One independent ffmpeg process per configured `StationHlsStream`**
//!   (bitrate-ladder rendition), each subscribing to the same
//!   `audio_tap` broadcast channel `output.rs`'s targets already use. No
//!   `share_encoders` support, consistent with every other output section
//!   in this engine.
//! - **Segmenting**: ffmpeg's native `-f hls` muxer writes `.ts` segments +
//!   a per-rendition `.m3u8` playlist directly under `HlsConfig::base_dir`.
//!   `-hls_flags delete_segments` plus `-hls_delete_threshold` (fed from
//!   SPEC.md's `segments_overhead`) handle the rolling window and cleanup
//!   natively -- no bespoke Rust segment-lifecycle logic needed.
//! - **Master playlist**: when more than one rendition is configured (a
//!   real bitrate ladder), [`write_master_playlist`] writes one
//!   `live.m3u8` referencing each rendition's own playlist by bandwidth --
//!   matching SPEC.md B.8's `playlist="live.m3u8"` naming and
//!   `Nginx\ConfigWriter`'s `{hlsBaseUrl}/live.m3u8` proxy target. Written
//!   once at startup (the rendition list is fixed for the process's
//!   lifetime; config isn't hot-reloaded), not as a recurring task.
//! - **Encoding: always AAC-LC**, regardless of what `HlsStreamProfiles`
//!   value a station nominally configures (`aac`/`aac_he`/`aac_he_v2`).
//!   ffmpeg's built-in `aac` encoder -- the encoder this engine uses
//!   everywhere, deliberately avoiding `libfdk_aac` for the same licensing/
//!   availability reasons `output.rs`'s module doc already explains for its
//!   own AAC output -- does not support the HE-AAC profiles at all. A
//!   station configured for HE-AACv2 still gets working AAC-LC HLS output
//!   here, not a build failure or a missing rendition; the master
//!   playlist's `CODECS` string is correspondingly always AAC-LC's
//!   (`mp4a.40.2`), since that's what's genuinely being produced.
//! - **Resilience**: each rendition retries forever with a fixed backoff on
//!   ffmpeg failure/exit, matching `output.rs::run_output_target`'s
//!   pattern -- one rendition's failure never affects any other rendition,
//!   any Icecast/relay output, or the AutoDJ/harbor pipeline.

use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tokio::sync::broadcast;

use crate::config::{HlsConfig, HlsStreamConfig};
use crate::decode::{PIPELINE_CHANNELS, PIPELINE_SAMPLE_RATE};

/// Fixed reconnect backoff after ffmpeg exits or fails, matching
/// `output.rs::RECONNECT_BACKOFF` -- see that module's doc for why a fixed
/// delay (no historical Liquidsoap behavior to match here) is appropriate.
const RECONNECT_BACKOFF: Duration = Duration::from_secs(5);

/// HLS master-playlist codec string. Always AAC-LC -- see this module's doc
/// for why every rendition is encoded as AAC-LC regardless of its
/// nominally-configured `HlsStreamProfiles` value.
const HLS_AAC_CODEC_STRING: &str = "mp4a.40.2";

/// Full ffmpeg CLI argument list for segmenting raw interleaved-`f32`-LE PCM
/// (read from stdin) into an HLS rendition written to `hls.base_dir`.
/// Pure/unit-testable: builds the argument vector without spawning
/// anything.
pub fn hls_ffmpeg_args(
    name: &str,
    bitrate_kbps: u32,
    hls: &HlsConfig,
    sample_rate: u32,
    channels: u16,
) -> Vec<String> {
    vec![
        "-hide_banner".to_string(),
        "-loglevel".to_string(),
        "error".to_string(),
        "-f".to_string(),
        "f32le".to_string(),
        "-ar".to_string(),
        sample_rate.to_string(),
        "-ac".to_string(),
        channels.to_string(),
        "-i".to_string(),
        "pipe:0".to_string(),
        "-c:a".to_string(),
        "aac".to_string(),
        "-b:a".to_string(),
        format!("{bitrate_kbps}k"),
        "-f".to_string(),
        "hls".to_string(),
        "-hls_time".to_string(),
        format!("{}", hls.segment_secs),
        "-hls_list_size".to_string(),
        hls.segments_in_playlist.to_string(),
        "-hls_delete_threshold".to_string(),
        hls.segments_overhead.to_string(),
        "-hls_flags".to_string(),
        "delete_segments".to_string(),
        "-hls_segment_filename".to_string(),
        format!("{}/{}_%d.ts", hls.base_dir, name),
        format!("{}/{}.m3u8", hls.base_dir, name),
    ]
}

fn build_ffmpeg_command(stream: &HlsStreamConfig, hls: &HlsConfig) -> Command {
    let mut cmd = Command::new("ffmpeg");
    cmd.args(hls_ffmpeg_args(
        &stream.name,
        stream.bitrate,
        hls,
        PIPELINE_SAMPLE_RATE,
        PIPELINE_CHANNELS,
    ));
    cmd.stdin(Stdio::piped());
    // Unlike output.rs's Icecast targets, there is nothing meaningful on
    // ffmpeg's stdout here -- HLS output goes entirely to the segment/
    // playlist files on disk, matching `-f hls`'s own muxer behavior.
    cmd.stdout(Stdio::null());
    cmd.stderr(Stdio::null());
    cmd
}

/// Writes the top-level multi-bitrate master playlist (`live.m3u8`,
/// matching SPEC.md B.8's `playlist="live.m3u8"` and
/// `Nginx\ConfigWriter::writeHlsSection`'s `{hlsBaseUrl}/live.m3u8` proxy
/// target) referencing each rendition's own per-bitrate playlist. Called
/// once at startup for the whole fixed rendition list -- see this module's
/// doc. A no-op returning `Ok(())` immediately if `streams` is empty (the
/// caller shouldn't have invoked HLS output at all in that case, but this
/// stays defensive rather than writing a playlist with zero variants).
pub fn write_master_playlist(hls: &HlsConfig, streams: &[HlsStreamConfig]) -> std::io::Result<()> {
    if streams.is_empty() {
        return Ok(());
    }

    let mut playlist = String::from("#EXTM3U\n#EXT-X-VERSION:3\n");
    for stream in streams {
        let bandwidth = stream.bitrate as u64 * 1000;
        playlist.push_str(&format!(
            "#EXT-X-STREAM-INF:BANDWIDTH={bandwidth},CODECS=\"{HLS_AAC_CODEC_STRING}\"\n{}.m3u8\n",
            stream.name
        ));
    }

    std::fs::create_dir_all(&hls.base_dir)?;
    std::fs::write(format!("{}/live.m3u8", hls.base_dir), playlist)
}

/// Runs `stream`'s HLS segmenter forever: spawn ffmpeg, pump the pipeline's
/// tapped PCM into it -- and on any failure (spawn, write, or ffmpeg simply
/// exiting), log it, wait `RECONNECT_BACKOFF`, and try again from the top.
/// Never returns; intended to be spawned as its own independent
/// `tokio::spawn` task per HLS rendition (see `main.rs`), so one
/// rendition's failure never affects any other rendition, any Icecast/relay
/// output, or the AutoDJ/harbor pipeline.
pub async fn run_hls_rendition(stream: HlsStreamConfig, hls: HlsConfig, tap: broadcast::Sender<Arc<Vec<f32>>>) {
    loop {
        match run_hls_once(&stream, &hls, &tap).await {
            Ok(()) => tracing::warn!(
                "hls[{}]: ffmpeg exited; restarting in {RECONNECT_BACKOFF:?}",
                stream.name
            ),
            Err(e) => tracing::error!(
                "hls[{}]: {e}; restarting in {RECONNECT_BACKOFF:?}",
                stream.name
            ),
        }
        tokio::time::sleep(RECONNECT_BACKOFF).await;
    }
}

/// One spawn-and-feed attempt for `stream`. Returns once ffmpeg's stdin
/// pipe breaks or the tap closes -- the caller (`run_hls_rendition`)
/// handles retry/backoff.
async fn run_hls_once(
    stream: &HlsStreamConfig,
    hls: &HlsConfig,
    tap: &broadcast::Sender<Arc<Vec<f32>>>,
) -> Result<(), String> {
    let mut rx = tap.subscribe();

    tracing::info!(
        "hls[{}]: starting ffmpeg segmenter -> {}/{}.m3u8",
        stream.name,
        hls.base_dir,
        stream.name
    );

    let mut child = build_ffmpeg_command(stream, hls)
        .spawn()
        .map_err(|e| format!("failed to spawn ffmpeg: {e}"))?;

    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| "ffmpeg child has no stdin handle".to_string())?;

    loop {
        match rx.recv().await {
            Ok(samples) => {
                let mut bytes = Vec::with_capacity(samples.len() * 4);
                for s in samples.iter() {
                    bytes.extend_from_slice(&s.to_le_bytes());
                }
                if let Err(e) = stdin.write_all(&bytes).await {
                    let _ = child.kill().await;
                    return Err(format!("failed writing to ffmpeg stdin: {e}"));
                }
            }
            Err(broadcast::error::RecvError::Lagged(n)) => {
                tracing::warn!(
                    "hls[{}]: feed lagged behind the pipeline by {n} message(s); \
                     continuing with newer audio",
                    stream.name
                );
                continue;
            }
            Err(broadcast::error::RecvError::Closed) => {
                // Dropping `stdin` closes ffmpeg's stdin, signaling EOF so
                // it flushes and exits cleanly.
                drop(stdin);
                let _ = child.wait().await;
                return Ok(());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_hls_config() -> HlsConfig {
        HlsConfig {
            base_dir: "/tmp/hls".to_string(),
            segment_secs: 4.0,
            segments_in_playlist: 5,
            segments_overhead: 2,
        }
    }

    // --- ffmpeg argument construction ---------------------------------------

    #[test]
    fn hls_ffmpeg_args_uses_aac_and_hls_muxer() {
        let hls = test_hls_config();
        let args = hls_ffmpeg_args("aac_lofi", 128, &hls, 44100, 2);
        assert!(args.windows(2).any(|w| w == ["-c:a", "aac"]));
        assert!(args.windows(2).any(|w| w == ["-b:a", "128k"]));
        assert!(args.windows(2).any(|w| w == ["-f", "hls"]));
        assert!(args.windows(2).any(|w| w == ["-ar", "44100"]));
        assert!(args.windows(2).any(|w| w == ["-ac", "2"]));
    }

    #[test]
    fn hls_ffmpeg_args_carries_segment_and_playlist_tuning() {
        let hls = test_hls_config();
        let args = hls_ffmpeg_args("aac_lofi", 128, &hls, 44100, 2);
        assert!(args.windows(2).any(|w| w == ["-hls_time", "4"]));
        assert!(args.windows(2).any(|w| w == ["-hls_list_size", "5"]));
        assert!(args.windows(2).any(|w| w == ["-hls_delete_threshold", "2"]));
        assert!(args.iter().any(|a| a == "delete_segments"));
    }

    #[test]
    fn hls_ffmpeg_args_segment_filename_and_playlist_use_the_rendition_name() {
        let hls = test_hls_config();
        let args = hls_ffmpeg_args("aac_lofi", 128, &hls, 44100, 2);
        let segment_filename_idx = args
            .iter()
            .position(|a| a == "-hls_segment_filename")
            .expect("should have -hls_segment_filename flag");
        assert_eq!(args[segment_filename_idx + 1], "/tmp/hls/aac_lofi_%d.ts");
        assert_eq!(args.last().unwrap(), "/tmp/hls/aac_lofi.m3u8");
    }

    // --- Master playlist construction (pure string building) ---------------

    #[test]
    fn master_playlist_is_empty_no_op_when_no_streams() {
        let dir = std::env::temp_dir().join(format!("hls-test-empty-{}", std::process::id()));
        let hls = HlsConfig {
            base_dir: dir.to_string_lossy().to_string(),
            ..test_hls_config()
        };
        write_master_playlist(&hls, &[]).expect("should succeed as a no-op");
        assert!(!dir.join("live.m3u8").exists());
    }

    #[test]
    fn master_playlist_lists_every_rendition_with_its_bandwidth() {
        let dir = std::env::temp_dir().join(format!("hls-test-{}", std::process::id()));
        let hls = HlsConfig {
            base_dir: dir.to_string_lossy().to_string(),
            ..test_hls_config()
        };
        let streams = vec![
            HlsStreamConfig {
                name: "aac_lofi".to_string(),
                bitrate: 64,
            },
            HlsStreamConfig {
                name: "aac_hifi".to_string(),
                bitrate: 256,
            },
        ];

        write_master_playlist(&hls, &streams).expect("should write master playlist");

        let content = std::fs::read_to_string(dir.join("live.m3u8")).expect("should read it back");
        assert!(content.starts_with("#EXTM3U\n"));
        assert!(content.contains("BANDWIDTH=64000"));
        assert!(content.contains("aac_lofi.m3u8"));
        assert!(content.contains("BANDWIDTH=256000"));
        assert!(content.contains("aac_hifi.m3u8"));

        std::fs::remove_dir_all(&dir).ok();
    }

    // --- Real ffmpeg integration check (only if ffmpeg is on PATH) ---------

    /// Mirrors `output.rs`'s own real-ffmpeg check: if `ffmpeg` is actually
    /// available, segment one second of synthetic silent PCM into an HLS
    /// rendition and confirm a playlist + at least one segment file were
    /// actually produced on disk. Skips (rather than failing or fabricating
    /// success) if `ffmpeg` isn't found.
    #[tokio::test]
    async fn segments_synthetic_pcm_via_real_ffmpeg_if_available() {
        let has_ffmpeg = Command::new("ffmpeg")
            .arg("-version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await
            .map(|s| s.success())
            .unwrap_or(false);

        if !has_ffmpeg {
            eprintln!("ffmpeg not found on PATH; skipping real-segment integration check");
            return;
        }

        let dir = std::env::temp_dir().join(format!("hls-test-real-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let hls = HlsConfig {
            base_dir: dir.to_string_lossy().to_string(),
            segment_secs: 1.0,
            segments_in_playlist: 5,
            segments_overhead: 2,
        };
        let stream = HlsStreamConfig {
            name: "test_stream".to_string(),
            bitrate: 64,
        };

        let mut child = build_ffmpeg_command(&stream, &hls)
            .spawn()
            .expect("ffmpeg should spawn");
        let mut stdin = child.stdin.take().unwrap();

        // A couple of seconds of silence, enough for ffmpeg's HLS muxer to
        // flush at least one full segment.
        let frames = PIPELINE_SAMPLE_RATE as usize * 2;
        let pcm = vec![0.0f32; frames * PIPELINE_CHANNELS as usize];
        let mut bytes = Vec::with_capacity(pcm.len() * 4);
        for s in &pcm {
            bytes.extend_from_slice(&s.to_le_bytes());
        }
        stdin.write_all(&bytes).await.expect("should write pcm to ffmpeg stdin");
        drop(stdin);

        let _ = child.wait().await;

        let playlist_path = dir.join("test_stream.m3u8");
        assert!(playlist_path.exists(), "ffmpeg should have written a playlist file");

        let has_segment = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .any(|e| e.path().extension().is_some_and(|ext| ext == "ts"));
        assert!(has_segment, "ffmpeg should have written at least one .ts segment");

        std::fs::remove_dir_all(&dir).ok();
    }
}
