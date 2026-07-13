# AzuraCast Liquidsoap Integration — Behavioral Specification

Extracted by reading (not running) the following source files in the `azuracast-bsd` repository:

- `backend/src/Radio/Backend/Liquidsoap/ConfigWriter.php`
- `util/docker/stations/liquidsoap/azuracast.liq`, `crossfade.liq`, `utilities.liq`
- `backend/src/Radio/Backend/Liquidsoap/Command/{AbstractCommand,NextSongCommand,DjAuthCommand,DjOnCommand,DjOffCommand,FeedbackCommand,CopyCommand,SaveCacheCommand}.php`
- `backend/src/Entity/StationBackendConfiguration.php`
- Supporting files read for exactness (routing, auth, enums, and the PHP-side collaborators the commands call into): `backend/src/Controller/Api/Internal/LiquidsoapAction.php`, `backend/config/routes/api_internal.php`, `backend/src/Middleware/RequireInternalConnection.php`, `backend/src/Radio/Enums/LiquidsoapCommands.php`, `backend/src/Entity/Station.php` (relevant fields only), `backend/src/Entity/StationStreamer.php`, `backend/src/Entity/Repository/StationStreamerRepository.php`, `backend/src/Radio/AutoDJ/Scheduler.php`, `backend/src/Radio/AutoDJ/Annotations.php`, `backend/src/Event/Radio/AnnotateNextSong.php`, `backend/src/Entity/StationPlaylist.php` and its enums, `backend/src/Radio/StereoTool.php`, `backend/src/Radio/Enums/{CrossfadeModes,AudioProcessingMethods,MasterMePresets,StationBackendPerformanceModes,LiquidsoapQueues,StreamFormats,StreamProtocols,FrontendAdapters}.php`, `backend/src/Sync/Task/{EnforceBroadcastTimesTask,ReactivateStreamerTask}.php`.

---

## A. DB-Stored Settings Reference

### A.1 `StationBackendConfiguration` (the primary Liquidsoap-specific settings blob, stored as JSON array on the station)

| Field | Type | Default | Controls |
|---|---|---|---|
| `charset` | string | `'UTF-8'` | ICY/metadata charset passed to `harbor`/`output.icecast`/`output.shoutcast` (`icy_metadata_charset`, `metadata_charset`, `encoding`). |
| `dj_port` | ?int | null | *(Not read by ConfigWriter — the actual harbor listen port comes from `Liquidsoap::getStreamPort($station)`, a different subsystem. Present on this entity as a stored value but not consumed in the reviewed code path.)* |
| `telnet_port` | ?int | null | *(Not referenced in ConfigWriter; legacy/telnet port storage — actual Liquidsoap control now goes through the HTTP API port, see below.)* |
| `record_streams` | bool | false | If true, `writeHarborConfiguration` adds an `output.file(...)` block recording the live harbor input to disk. |
| `record_streams_format` | string (StreamFormats) | `''` → `Mp3` via `getRecordStreamsFormatEnum()` | Container/codec for the live recording. |
| `record_streams_bitrate` | int | 128 | Bitrate for the live recording encoder. |
| `use_manual_autodj` | bool | false | If true: (1) `azuracast.enable_autodj(radio)` wrapping is **skipped** in `writePlaylistConfiguration` (station is expected to be driven by an external/manual AutoDJ trigger rather than Liquidsoap's own `request.dynamic` loop); (2) `shouldWritePlaylist()` forces **all** playlists to be written into the `.liq` file regardless of the "only write if needed" optimization. |
| `autodj_queue_length` | int | 3 | *(Not referenced in ConfigWriter's reviewed methods — consumed elsewhere, e.g. queue-building logic, out of scope of this file set.)* |
| `dj_mount_point` | string | `'/'` | Mount point string passed as the first positional arg to `input.harbor(...)`. |
| `dj_buffer` | int | 5 | If non-zero, sets `buffer=` (as float) and `max = max(buffer+5, 10)` (as float) on `input.harbor(...)`. If `0`, neither param is emitted (Liquidsoap harbor defaults apply). |
| `audio_processing_method` | string (AudioProcessingMethods) | `''` → `None` via `getAudioProcessingMethodEnum()` | Selects the post-processing branch in `writePostProcessingSection` (`none` / `nrj` / `master_me` / `stereo_tool`). |
| `post_processing_include_live` | bool | false | Determines **where** `writePostProcessingSection` is invoked: if false, processing runs in `writeCrossfadeConfiguration` (before the live-DJ harbor fallback is layered in, i.e. live broadcasts bypass it); if true, it runs in `writePreBroadcastConfiguration` (after live is merged in, i.e. live broadcasts are also processed). |
| `stereo_tool_license_key` | ?string | null | License key argument passed to the Stereo Tool binary/library invocation. |
| `stereo_tool_configuration_path` | ?string | null | Relative path (joined with the station's radio config dir) to a Stereo Tool preset/config file; also gates `StereoTool::isReady()` (must be non-empty). |
| `master_me_preset` | ?string (MasterMePresets) | null → `MusicGeneral` via `getMasterMePresetEnum()` | Selects the base parameter table (`getOptions()`) for `ladspa.master_me(...)`. |
| `master_me_loudness_target` | int | -16 | If non-zero, overrides the preset's `target` option. |
| `enable_replaygain_metadata` | bool | false | **Getter override:** always returns `false` if `enable_auto_cue` is true, regardless of stored value. When effectively true, `writeCrossfadeConfiguration` emits `enable_replaygain_metadata()` + wraps `radio = replaygain(radio)`. |
| `crossfade_type` | string (CrossfadeModes) | `''` | Raw stored value; see `getCrossfadeTypeEnum()` below for the effective value. |
| `crossfade` | float | 2.0 (`DEFAULT_CROSSFADE_DURATION`), rounded to 1 decimal | Fed as `settings.azuracast.default_fade` (fade-in/out duration) verbatim, and as the base for `getCrossfadeDuration()`. |
| `crossfade_smart_high` | float | -15.0 | `high` parameter of `cross.smart`. |
| `crossfade_smart_medium` | float | -32.0 | `medium` parameter of `cross.smart`. |
| `crossfade_smart_margin` | float | 8.0 | `margin` parameter of `cross.smart`. |
| `duplicate_prevention_time_range` | int | 120 | *(Not referenced in ConfigWriter — consumed by queue-building logic outside the reviewed file set.)* |
| `performance_mode` | string (StationBackendPerformanceModes) | `''` → `Disabled` via `getPerformanceModeEnum()` | Selects GC tuning block in `writeHeaderFunctions` (see B.1). |
| `hls_segment_length` | int | 4 | HLS segment duration (seconds) — used both in `hls_segment_name()` and `output.file.hls(segment_duration=...)`. |
| `hls_segments_in_playlist` | int | 5 | `output.file.hls(segments=...)`. |
| `hls_segments_overhead` | int | 2 | `output.file.hls(segments_overhead=...)`. |
| `hls_enable_on_public_player` | bool | false | *(Not referenced in ConfigWriter — consumed by frontend/player-facing code, out of scope.)* |
| `hls_is_default` | bool | false | *(Same — not part of `.liq` generation.)* |
| `live_broadcast_text` | string | `'Live Broadcast'` | Fed as `settings.azuracast.live_broadcast_text`; used as placeholder "now playing" title when a DJ connects without display name, and as fallback `display_name` in `azuracast.dj_auth`. |
| `enable_auto_cue` | bool | false | Fed as `settings.azuracast.compute_autocue`. Also forces `crossfade_type` effective value to `Disabled` (see `getCrossfadeTypeEnum()`) and forces `enable_replaygain_metadata` effective value to `false`. |
| `write_playlists_to_liquidsoap` | bool | false | If true, `shouldWritePlaylist()` always returns true (write every playlist into the `.liq` literally, rather than relying on the AutoDJ database queue for plain "Songs" playlists with no special backend options). |
| `share_encoders` | bool | false | Gates `writeEncodingConfiguration` entirely (returns immediately if false) and changes the shape of `writeLocalBroadcastConfiguration`/`writeRemoteBroadcastConfiguration`/`writeHlsBroadcastConfiguration` output (single shared `ffmpeg.encode.audio(...)` source copied via `%format.copy`/`%format.drop`, vs. one independent encoder per output). |
| `custom_config_top` (`CUSTOM_TOP`) | ?string | null | Injected verbatim at the very top of the file (in `writeHeaderFunctions`), before anything else. |
| `custom_config_pre_playlists` (`CUSTOM_PRE_PLAYLISTS`) | ?string | null | Injected at the start of `writePlaylistConfiguration`. |
| `custom_config_pre_live` (`CUSTOM_PRE_LIVE`) | ?string | null | Injected at the start of `writeHarborConfiguration` (only reached if `enable_streamers`). |
| `custom_config_pre_fade` (`CUSTOM_PRE_FADE`) | ?string | null | Injected at the start of `writeCrossfadeConfiguration`. |
| `custom_config` (`CUSTOM_PRE_BROADCAST`) | ?string | null | Injected at the end of `writePreBroadcastConfiguration`. |
| `custom_config_bottom` (`CUSTOM_BOTTOM`) | ?string | null | Injected in `writePostBroadcastConfiguration` (last event fired, priority -5). |

Custom-config injection is globally gated by the **instance-wide** setting `enable_liquidsoap_editing` (read via `SettingsAwareTrait`/`settingsRepo->readSettings()`, not on `StationBackendConfiguration`): if disabled, none of the six custom sections are written into the real config, even if `writeCustomConfigurationSection` is called (see B.11).

Derived/computed methods on this entity that ConfigWriter relies on (not raw fields, but exact semantics the Rust engine must reproduce):

- `getCrossfadeTypeEnum()`: returns `CrossfadeModes::Disabled` unconditionally if `enable_auto_cue` is true; otherwise `CrossfadeModes::tryFrom($crossfade_type) ?? CrossfadeModes::Normal`.
- `getCrossfadeDuration()`: `0` if `getCrossfadeTypeEnum() === Disabled` or `crossfade <= 0`; otherwise `round(crossfade * 1.5, 2)`.
- `isCrossfadeEnabled()`: `getCrossfadeDuration() > 0`.
- `isAudioProcessingEnabled()`: `getAudioProcessingMethodEnum() !== None`.
- `getRecordStreamsEncoding()`: `null` if `record_streams` is false; else a fresh `EncodingFormat(format: getRecordStreamsFormatEnum(), bitrate: record_streams_bitrate, subProfile: null)`.

### A.2 `CrossfadeModes` enum (`App\Radio\Enums\CrossfadeModes`)
`Normal = 'normal'`, `Smart = 'smart'`, `Disabled = 'none'`. Default: `Normal`.

### A.3 `AudioProcessingMethods` enum
`None = 'none'`, `Liquidsoap = 'nrj'`, `MasterMe = 'master_me'`, `StereoTool = 'stereo_tool'`. Default: `None`.

### A.4 `StationBackendPerformanceModes` enum
`LessMemory = 'less_memory'`, `LessCpu = 'less_cpu'`, `Balanced = 'balanced'`, `Disabled = 'disabled'`. Default: `Disabled`.

### A.5 `MasterMePresets` enum and parameter tables
Cases: `MusicGeneral`, `SpeechGeneral`, `EbuR128`, `ApplePodcasts`, `YouTube` (default: `MusicGeneral`). Each preset's `getOptions()` starts from a shared 60-key default table and overrides a subset of keys. **Full default table** (all float/int/bool values as literally declared in PHP):

```
bypass=false, target=-16, brickwall_bypass=false, brickwall_ceiling=-1.0, brickwall_release=75.0,
eq_bypass=false, eq_highpass_freq=5.0, eq_side_bandwidth=1.0, eq_side_freq=600.0, eq_side_gain=1.0,
eq_tilt_gain=0.0, gate_attack=0.0, gate_bypass=true, gate_hold=50.0, gate_release=430.5,
gate_threshold=-90.0, kneecomp_attack=20.0, kneecomp_bypass=false, kneecomp_dry_wet=50,
kneecomp_ff_fb=50, kneecomp_knee=6.0, kneecomp_link=60, kneecomp_makeup=0.0, kneecomp_release=340.0,
kneecomp_strength=20, kneecomp_tar_thresh=-4.0, leveler_brake_threshold=-10.0, leveler_bypass=false,
leveler_max=10.0, leveler_max__=10.0, leveler_speed=20, limiter_attack=3.0, limiter_bypass=false,
limiter_ff_fb=50, limiter_knee=3.0, limiter_makeup=0.0, limiter_release=40.0, limiter_strength=80,
limiter_tar_thresh=6.0, mscomp_bypass=false, high_attack=8.0, high_crossover=8000.0, high_knee=12.0,
high_link=30, high_release=30.0, high_strength=30, high_tar_thresh=-12.0, low_attack=15.0,
low_crossover=60.0, low_knee=12.0, low_link=70, low_release=150.0, low_strength=10,
low_tar_thresh=-3.0, makeup=1.0, dc_blocker=false, input_gain=0.0, mono=false, phase_l=false,
phase_r=false, stereo_correct=false
```

Per-preset overrides on top of the defaults:

- **MusicGeneral**: no overrides (pure defaults).
- **SpeechGeneral**: `eq_highpass_freq=20.0, eq_side_gain=0.0, gate_attack=1.0, gate_release=500.0, kneecomp_attack=5.0, kneecomp_knee=9.0, kneecomp_release=50.0, kneecomp_strength=15, kneecomp_tar_thresh=-6.0, leveler_brake_threshold=-20.0, leveler_max=30.0, leveler_max__=30.0, limiter_attack=1.0, limiter_strength=80, limiter_tar_thresh=3.0, high_attack=0.0, high_release=50.0, high_strength=40, high_tar_thresh=-7.0, low_attack=10.0, low_release=80.0, low_strength=20, low_tar_thresh=-5.0, dc_blocker=true`.
- **EbuR128**: `target=-23, eq_highpass_freq=20.0, eq_side_gain=0.0, gate_attack=1.0, gate_release=500.0, kneecomp_attack=5.0, kneecomp_bypass=true, kneecomp_knee=9.0, kneecomp_release=50.0, kneecomp_strength=15, kneecomp_tar_thresh=-6.0, leveler_brake_threshold=-20.0, leveler_max=30.0, leveler_max__=30.0, leveler_speed=40, limiter_attack=1.0, limiter_tar_thresh=3.0, high_attack=0.0, high_release=50.0, high_strength=40, high_tar_thresh=-8.0, low_attack=10.0, low_release=80.0, low_strength=20, low_tar_thresh=-6.0, dc_blocker=true`.
- **ApplePodcasts**: identical shape to EbuR128 but `target` stays at whatever the default table has (not overridden here — only `bypass=false` restated) and `leveler_speed=50`; other listed keys: `eq_highpass_freq=20.0, eq_side_gain=0.0, gate_attack=1.0, gate_release=500.0, kneecomp_attack=5.0, kneecomp_bypass=true, kneecomp_knee=9.0, kneecomp_release=50.0, kneecomp_strength=15, kneecomp_tar_thresh=-6.0, leveler_brake_threshold=-20.0, leveler_max=30.0, leveler_max__=30.0, limiter_attack=1.0, limiter_tar_thresh=3.0, high_attack=0.0, high_release=50.0, high_strength=40, high_tar_thresh=-8.0, low_attack=10.0, low_release=80.0, low_strength=20, low_tar_thresh=-6.0, dc_blocker=true`.
- **YouTube**: `target=-14, eq_highpass_freq=20.0, eq_side_gain=0.0, gate_attack=1.0, gate_release=500.0, kneecomp_attack=5.0, kneecomp_bypass=true, kneecomp_knee=9.0, kneecomp_release=50.0, kneecomp_strength=15, kneecomp_tar_thresh=-6.0, leveler_brake_threshold=-20.0, leveler_max=30.0, leveler_max__=30.0, leveler_speed=50, limiter_attack=1.0, limiter_tar_thresh=3.0, high_attack=0.0, high_strength=40, high_tar_thresh=-8.0, low_attack=10.0, low_release=80.0, low_strength=20, low_tar_thresh=-6.0, dc_blocker=true` (note: YouTube omits a `high_release` override, so it keeps the default-table `high_release=30.0`, unlike the other three which set it to `50.0`).

If `master_me_loudness_target != 0`, `ConfigWriter::writePostProcessingSection` overwrites the preset's `target` key with that value after computing preset options.

Value formatting rule when emitting the `ladspa.master_me(...)` call: ints → `toFloat(val, 0)` (i.e. printed as an integer-valued float string, no decimals — e.g. `50` not `50.00`), floats → `toFloat(val)` (2 decimals), bools → `true`/`false` literal, everything else passed through as-is.

### A.6 Station-level fields consumed by `ConfigWriter` (not on `StationBackendConfiguration`, listed for completeness since the Rust engine needs them too)

| Field/accessor | Controls |
|---|---|
| `station->timezone` | `environment.set("TZ", ...)`. |
| `station->adapter_api_key` | Sent as `settings.azuracast.api_key` and validated by `LiquidsoapAction`/`validateAdapterApiKey()` against the `X-Liquidsoap-Api-Key` header on every callback. |
| `station->media_storage_location` | If the adapter `isLocal()`, `settings.azuracast.media_path` is set to the filtered local path; otherwise `"api"` (forces the `media:` protocol to go through the `cp` HTTP callback instead of reading files directly). |
| `station->enable_streamers` | Gates `writeHarborConfiguration` entirely. |
| `station->backend_config->use_manual_autodj` | See A.1. |
| `station->frontend_type` | If `FrontendAdapters::Remote`, `writeLocalBroadcastConfiguration` is skipped entirely. |
| `station->enable_hls` | Gates `writeHlsBroadcastConfiguration`. |
| `station->mounts`, `station->remotes`, `station->hls_streams` | Iterated to build encoders/outputs. |
| `station->playlists` | Iterated to build the playlist/schedule graph. |
| `station->name`, `station->description`, `station->genre`, `station->url` | Emitted as `name=`/`description=`/`genre=`/`url=` on each `output.icecast`/`output.shoutcast` call. |
| `station->getRadioConfigDir()`, `getRadioTempDir()`, `getRadioHlsDir()` | Filesystem paths embedded in the generated config (pidfile, HLS persist/segment dirs, Stereo Tool config path join, temp path). |
| `station->disconnect_deactivate_streamer` (int, default 0) | **Not** referenced by the `.liq` runtime or by `DjOnCommand`/`DjOffCommand`. It is consumed only by `Liquidsoap::disconnectStreamer()` (an admin-triggered "kick DJ" action, and by `EnforceBroadcastTimesTask` when a streamer's schedule window ends): if `>0`, the currently-connected `StationStreamer` is deactivated (`is_active=false`, `reactivate_at = now()+seconds`) *in addition to* Liquidsoap being told `input_streamer.stop` over the telnet/HTTP command channel. A separate cron-style task, `ReactivateStreamerTask`, flips streamers back to `is_active=true` once `reactivate_at <= now()`. This is a PHP-side cooldown, not something the `.liq` scripts implement. |

### A.7 `StationPlaylist` fields/methods consumed by `writePlaylistConfiguration`

| Field/method | Meaning |
|---|---|
| `is_enabled` | Playlists with `false` are skipped entirely. |
| `source` (`PlaylistSources`: `Songs`/`RemoteUrl`) | Selects between a local `playlist()` object and remote-URL handling. |
| `remote_type` (`PlaylistRemoteTypes`: `Stream`/`Playlist`/`Other`) | `Playlist` → wraps `remote_url` in Liquidsoap's own `playlist(url)`; `Stream` → `input.http`; `Other` → `input.ffmpeg`. |
| `remote_url`, `remote_buffer` (default `StationPlaylist::DEFAULT_REMOTE_BUFFER = 20`, floor of 1) | Remote stream source config; wrapped `mksafe(buffer(buffer=N., input.X(url)))`. |
| `order` (`PlaylistOrders`: `Sequential`/`Shuffle`/`Random`) | Maps to Liquidsoap `playlist(mode=...)`: `Sequential→"normal"`, `Shuffle→"randomize"`, `Random→"random"`. `reload_mode="watch"` is always set. |
| `is_jingle` | Wraps the playlist var in `azuracast.utilities.drop_metadata(...)` (metadata is stripped entirely at the source level) — only applies to directly-written `playlist()`/`merge_tracks()` sources, not to jingle tracks delivered through the AutoDJ database queue (see C.7 for the separate `jingle_mode` runtime mechanism). |
| `type` (`PlaylistTypes`: `Standard='default'`, `OncePerXSongs`, `OncePerXMinutes`, `OncePerHour`, `Advanced='custom'`) | Drives the entire scheduling-graph construction (see B.2). `Advanced` playlists are `ignore()`d (their contents are managed entirely by custom `.liq` code injected via custom-config sections, not by generated logic). |
| `schedule_items` (Collection of `StationSchedule`) | If present, the playlist plays only during computed time windows (see `getScheduledPlaylistPlayTime`); if empty, it participates in the "standard" weighted-random rotation or special-playlist chain instead. |
| `weight` (int, default 3, floor of 1 via getter) | Relative weight in `random(weights=[...], [...])` for playlists with no schedule items. |
| `play_per_songs` (int) | For `OncePerXSongs`: interleave ratio in `rotate(weights=[1, N], [playlist, radio])`. |
| `play_per_minutes` (int) | For `OncePerXMinutes`: delay in seconds (`× 60`) before the playlist track is allowed via `fallback([delay(N., playlist), radio])`. |
| `play_per_hour_minute` (int, 0–59) | For `OncePerHour`: minute-of-hour trigger, combined with schedule/time predicate via `and`. |
| `backend_options` (comma list; constants `OPTION_INTERRUPT_OTHER_SONGS='interrupt'`, `OPTION_PLAY_SINGLE_TRACK='single_track'`, `OPTION_MERGE='merge'`, `OPTION_PRIORITIZE_OVER_REQUESTS='prioritize'`) | `backendInterruptOtherSongs()` → routed into the `track_sensitive=false` "interrupting" switch chain instead of the normal `track_sensitive=true` one; `backendPlaySingleTrack()` → wraps the time predicate in `predicate.at_most(1, {...})` (fires only once per window); `backendMerge()` → wraps the playlist in `merge_tracks(id="merge_X", X)`. (`prioritize` is stored but **not** read anywhere in `ConfigWriter` — out of scope of `.liq` generation.) |

Enum values, verbatim:
- `PlaylistSources`: `Songs='songs'`, `RemoteUrl='remote_url'`.
- `PlaylistRemoteTypes`: `Stream='stream'`, `Playlist='playlist'`, `Other='other'`.
- `PlaylistOrders`: `Random='random'`, `Shuffle='shuffle'`, `Sequential='sequential'`.
- `PlaylistTypes`: `Standard='default'`, `OncePerXSongs='once_per_x_songs'`, `OncePerXMinutes='once_per_x_minutes'`, `OncePerHour='once_per_hour'`, `Advanced='custom'`.

### A.8 Output/encoding fields (via `OutputtableSource`/`EncodingFormat`, consumed by `getOutputString`/`getFfmpegAudioString`)

Per mount/remote/HLS-stream: `host`, `port`, `username`, `password` (Shoutcast: `password .= ':#' . $id`), `mount`, `protocol` (`StreamProtocols`: `Icy='icy'`, `Http='http'`, `Https='https'` — `Https` adds `transport = https_transport`), `adapterType`/`isShoutcast` (`FrontendAdapters`: `Icecast='icecast'`, `Shoutcast='shoutcast2'`, `Rsas='rsas'`, `Remote='remote'` — selects `output.shoutcast` vs `output.icecast`), `isPublic`. Encoding (`EncodingFormat{format: StreamFormats, bitrate, subProfile}`, `StreamFormats`: `Mp3`,`Ogg`,`Aac`,`Opus`,`Flac`): `getFfmpegContainer()` → `mp3`→`mp3`, `Aac`→`adts`, else `ogg`; `sendIcyMetadata()` → `Flac`→`true` else `null` (param omitted); exact per-format `%audio(...)` strings are hardcoded in `getFfmpegAudioString` (see B.6/B.9 for the literal strings).

---

## B. ConfigWriter — Per-Section Behavior

`ConfigWriter` is a Symfony event subscriber on `WriteLiquidsoapConfiguration`; methods run in **descending priority order** (higher number first): `writeHeaderFunctions`(35) → `writePlaylistConfiguration`(30) → `writeCrossfadeConfiguration`(25) → `writeHarborConfiguration`(20) → `writePreBroadcastConfiguration`(10) → `writeEncodingConfiguration`(7) → `writeLocalBroadcastConfiguration`(5) → `writeHlsBroadcastConfiguration`(2) → `writeRemoteBroadcastConfiguration`(0) → `writePostBroadcastConfiguration`(-5). This ordering is the exact sequence the Rust engine's generator must reproduce when concatenating output blocks (each handler appends lines/blocks to a shared buffer via `$event->appendLines()`/`appendBlock()`/`prependLines()`).

### B.1 `writeHeaderFunctions`
1. If not in "for-editing" mode (`!$event->isForEditing()`), prepends a two-line "auto-generated, do not edit" warning as the very first lines of the file.
2. Writes the `CUSTOM_TOP` custom-config section (see B.11).
3. Emits a fixed header block:
   - `%include` of the shared `azuracast.liq` (absolute path: `{parent_dir}/liquidsoap/azuracast.liq`).
   - `log.level := 3` in production, `4` otherwise (`environment.isProduction()`).
   - `init.daemon.pidfile.path := "{config_dir}/liquidsoap.pid"`.
   - `settings.init.compact_before_start := true` (always).
   - `environment.set("TZ", station.timezone)`.
   - `settings.azuracast.liquidsoap_api_port := {http_api_port}` (from `Liquidsoap::getHttpApiPort($station)`).
   - `settings.azuracast.api_url := "{internal_uri}/api/internal/{station_id}/liquidsoap"`.
   - `settings.azuracast.api_key := "{adapter_api_key}"`.
   - `settings.azuracast.media_path := "{local_path_or_'api'}"`.
   - `settings.azuracast.fallback_path := "{fallback_file_path}"` (from `FallbackFile::getFallbackPathForStation`).
   - `settings.azuracast.temp_path := "{radio_temp_dir}"`.
   - `settings.azuracast.compute_autocue := {true|false}` = `backend_config->enable_auto_cue`.
   - `settings.azuracast.default_fade := {crossfade}` (raw `crossfade` value, 2-decimal float string).
   - `settings.azuracast.default_cross := {getCrossfadeDuration()}` (the 1.5×-scaled value).
   - `settings.azuracast.enable_crossfade := {isCrossfadeEnabled()}`.
   - `settings.azuracast.crossfade_type := "smart"|"normal"` (from `getCrossfadeTypeEnum()`; note it's only ever `"smart"` or `"normal"` here even though the enum also has `Disabled` — `Disabled` collapses to `"normal"` string-wise, but `enable_crossfade` will be `false` in that case so it doesn't matter).
   - `settings.azuracast.crossfade_smart_high/medium/margin := ...`.
   - `settings.azuracast.live_broadcast_text := "{live_broadcast_text}"`.
   - `azuracast.start_http_api()` call (starts the Liquidsoap-side HTTP server that receives telnet-equivalent commands like `skip`, `custom_metadata.insert`).
   - Note: `settings.azuracast.request_timeout`, `settings.azuracast.http_timeout`, and `settings.azuracast.apply_amplify` are **never** written by this method — they always retain the hardcoded defaults declared in `azuracast.liq` (`20.0`, `10.0`, `true` respectively) unless a station's custom-config injection overrides them explicitly.
4. If `performance_mode != Disabled`, appends a `runtime.gc.set(runtime.gc.get().{space_overhead=N, allocation_policy=2})` block, where `N` = 20 (LessMemory), 140 (LessCpu), 80 (Balanced). Nothing is emitted for `Disabled`.

### B.2 `writePlaylistConfiguration`
Writes the `CUSTOM_PRE_PLAYLISTS` section, then builds the entire playlist/rotation/scheduling graph:

- For each enabled playlist that passes `shouldWritePlaylist()` (see B.13):
  - Compute a unique Liquidsoap variable name via `cleanUpVarName('playlist_' . StationPlaylist::generateShortName($name))`; if a collision occurs with an already-used name, suffix `_ {playlist->id}`.
  - **Songs source**: builds `playlist(id=..., mime_type="audio/x-mpegurl", mode="normal|randomize|random", reload_mode="watch", "{playlist_file_path}")` (file path from `PlaylistFileWriter::getPlaylistFilePath`, one URI per line, `reload_mode="watch"` means Liquidsoap re-reads the file automatically on external changes rather than needing a script reload). If `backendMerge()`, wraps in `merge_tracks(id="merge_{name}", {name})`.
  - **Remote source, type=Playlist**: `{name} = playlist("{remote_url}")` (delegates iteration entirely to Liquidsoap's playlist reader against the remote URL).
  - **Remote source, type≠Playlist (Stream/Other)**: builds `mksafe(buffer(buffer=N., input.http(url)))` (Stream) or `mksafe(buffer(buffer=N., input.ffmpeg(url)))` (Other). If the playlist has **no schedule items**, this becomes the single station-wide `fallbackRemoteUrl` (only the *last* such playlist wins — each new one silently overwrites `$fallbackRemoteUrl`); if it has schedule items, it's written as a named var and its play windows are pushed into a separate `scheduleSwitchesRemoteUrl` list (handled at the very end, after requests). If `remote_url` is null, the playlist is skipped outright (`continue`).
  - If `is_jingle`, the (non-remote-stream) playlist var is wrapped: `{name} = azuracast.utilities.drop_metadata({name})`.
  - If `type === Advanced`, the var is wrapped in `ignore({name})` (i.e. defined but not incorporated into radio's automatic rotation — expected to be consumed by custom injected code).
  - Dispatch by `type`:
    - **Standard**: if it has schedule items, for each schedule item compute the play-time predicate (`getScheduledPlaylistPlayTime`, see B.12) and build a `(predicate, name)` switch tuple — `predicate.at_most(1, {...})` wrapping if `backendPlaySingleTrack()`, else the bare `{...}` time predicate. Routed into `scheduleSwitchesInterrupting` if `backendInterruptOtherSongs()`, else `scheduleSwitches`. If no schedule items, it's added to the weighted "standard playlists" pool (`genPlaylistWeights`/`genPlaylistVars`).
    - **OncePerXSongs**: builds `rotate(weights=[1, play_per_songs], [{name}, radio])` as the effective schedulable unit; same schedule-item/no-schedule-item branching as Standard, except with no schedule items it's stored under the `once_per_x_songs` special-playlist bucket as `radio = {rotate expr}` (re-assigning `radio` itself, meaning it wraps whatever `radio` currently is at that point in file order).
    - **OncePerXMinutes**: builds `fallback(track_sensitive={not interrupt}, [delay(play_per_minutes*60., {name}), radio])`; same branching, no-schedule-item case stored under `once_per_x_minutes` bucket as `radio = {fallback expr}`.
    - **OncePerHour**: play-time predicate is `(play_per_hour_minute + "m") and (scheduled window, if any)`; always goes into `scheduleSwitches`/`scheduleSwitchesInterrupting` (even with zero schedule items, it still produces exactly one switch tuple using just the minute-of-hour predicate).
    - **Advanced**: no-op (handled entirely by the `ignore()` wrap above).
- **Standard playlists pool**: always emits `radio = random(id="standard_playlists", weights=[...], [...])` — even if both lists are empty (produces `random(id="standard_playlists", weights=[], [])`, i.e. a source with no children; this is a real edge case if a station has zero un-scheduled Standard playlists).
- **Schedule switches**: if `scheduleSwitches` non-empty, chunks the list into groups of ≤168 entries (`array_chunk(..., 168, true)`) — this is a hard Liquidsoap `switch()` list-size consideration — appending `({true}, radio)` as the catch-all tail of each chunk, and re-assigning `radio = switch(id="schedule_switch", track_sensitive=true, [...])` once per chunk (so multiple chunks nest one inside the next via successive reassignment of `radio`).
- **Special playlists** (`once_per_x_songs`/`once_per_x_minutes` buckets): only written if more than just the header comment line was accumulated (i.e. at least one entry was added).
- **Interrupting schedule switches**: same chunking logic as above but `track_sensitive=false` and using `scheduleSwitchesInterrupting`.
- **AutoDJ wrap**: unless `use_manual_autodj`, `radio = azuracast.enable_autodj(radio)` (see C.3).
- **Remote-URL fallback**: if a `fallbackRemoteUrl` was captured, `remote_url = {expr}` then `radio = fallback(id="fallback_remote_url", track_sensitive=false, [remote_url, radio])`.
- **Request queues** (always emitted, unconditionally): `requests = request.queue(id="requests", timeout=settings.azuracast.request_timeout())` then `radio = fallback(id="requests_fallback", track_sensitive=true, [requests, radio])`; and `interrupting_queue = request.queue(id="interrupting_requests", timeout=...)` then `radio = fallback(id="interrupting_fallback", track_sensitive=false, [interrupting_queue, radio])`. (Queue IDs come from `LiquidsoapQueues::Requests='requests'` / `Interrupting='interrupting_requests'`.)
- **Remote-URL schedule switches** (playlists of remote-stream type *with* schedule items, collected earlier): same 168-chunking + `({true}, radio)` tail pattern, `track_sensitive=false`, applied last — i.e. remote-URL scheduled overrides sit at the *outermost* layer of the whole rotation graph, above even the request queues.

### B.3 `writeCrossfadeConfiguration`
1. Writes `CUSTOM_PRE_FADE` section.
2. Always emits: `azuracast.utilities.add_skip_command(radio)` (registers a `radio skip` telnet/HTTP command); `radio = azuracast.apply_amplify(radio)` (see C — no-op unless `settings.azuracast.apply_amplify` is true, which it is by default and not station-configurable via ConfigWriter).
3. If `enable_replaygain_metadata` (effective, i.e. false when `enable_auto_cue`): `enable_replaygain_metadata()` + `radio = replaygain(radio)`.
4. Always: `source.methods(radio).on_metadata(synchronous=false, azuracast.log_meta)` (debug logging tap) then `radio = azuracast.apply_crossfade(radio)` (see C.5).
5. If `isAudioProcessingEnabled() && !post_processing_include_live`, calls `writePostProcessingSection` **here** — i.e. audio processing is applied *before* the live-DJ harbor source is merged in (live broadcasts bypass this processing chain entirely in this configuration).

### B.4 `writeHarborConfiguration`
Returns immediately (no-op) if `!station->enable_streamers`. Otherwise:
1. Writes `CUSTOM_PRE_LIVE`.
2. Builds `input.harbor(mount, id="input_streamer", port={stream_port}, auth=azuracast.dj_auth, icy=true, icy_metadata_charset="{charset}", metadata_charset="{charset}"[, buffer=N., max=max(N+5,10).])` — buffer/max only added if `dj_buffer != 0`.
3. Emits:
   - `radio_without_live = radio` (a reference to the pre-live chain, kept alive with `ignore()` so it isn't garbage-collected/pruned).
   - `live = input.harbor(...)`.
   - `live.on_connect(synchronous=false, azuracast.live_connected)`, `live.on_disconnect(synchronous=false, azuracast.live_disconnected)`.
   - A `def insert_missing(m)` function that would inject `("title", live_broadcast_text)` + `("is_live","true")` when metadata is empty, or just `("is_live","true")` otherwise — **but the line that would actually apply this (`live = metadata.map(insert_missing, live)`) is commented out** in the generated output ("Temporarily disabled for testing"), so **`is_live` metadata is never actually attached in the current implementation** — this is a real, verifiable no-op the Rust engine must match (i.e. do NOT implement `insert_missing`'s effect unless explicitly asked to fix this known dead code).
   - `radio = fallback(id="live_fallback", track_sensitive=true, replay_metadata=true, transitions=[fun(_,s)->{log(...); s}, fun(_,s)->s], [live, radio])` — live takes priority whenever ready; the two transition functions are functionally identical (both just return `s`) except the first logs "executing transition to live"; net effect: no actual audio transition/fade logic here beyond what `apply_crossfade`'s live-aware wrapper (C.5) handles further up the chain, since `apply_crossfade` was already applied to `radio` in step B.3 — but note `live` itself is *not* passed through crossfade, only `radio` (the pre-live/AutoDJ mix) was.
   - `check_live()`: on every ready-check, if `live.is_ready()` and not already flagged `azuracast.to_live`, sets `azuracast.to_live := true` and calls `radio_without_live.skip()` (forces the AutoDJ source to skip its current track so it doesn't "come back" mid-track once live ends); if not ready, resets `azuracast.to_live := false`. Registered via `source.methods(radio).on_frame(synchronous=true, check_live)` — runs synchronously on every audio frame of the composed `radio` source.
4. If `record_streams`: adds `output.file(%ffmpeg(...), fun() -> if azuracast.live_enabled() then "{temp_path}/{live_dj}/{PATH_PREFIX}_%Y%m%d-%H%M%S.{ext}.tmp" else "" end, live, fallible=true, on_close=fun(tempPath) -> {rename tempPath to strip trailing ".tmp" via `mv`, logged})`. The recording filename embeds the currently-connected DJ's username (`azuracast.live_dj()`) and a timestamp; when the file is closed, a `.tmp`-stripped rename is performed via shelling out to `mv`.

### B.5 `writePreBroadcastConfiguration`
1. Always: `azuracast.utilities.add_custom_metadata_command(id="custom_metadata", radio)` (registers telnet/HTTP `custom_metadata.insert key="val",...` command, see C.9).
2. If `isAudioProcessingEnabled() && post_processing_include_live`, calls `writePostProcessingSection` **here** instead (i.e. after live is merged, so live broadcasts *are* processed too, in this configuration).
3. Always:
   - `radio = azuracast.add_fallback(radio)` — final safety-net wrap (see C.8).
   - `source.methods(radio).on_metadata(synchronous=false, azuracast.send_feedback)` — pushes now-playing metadata changes back to AzuraCast via the `feedback` HTTP callback (C.6/D.5).
   - `radio = azuracast.handle_jingle_mode(radio)` — runtime jingle-mode suppression (C.7).
4. Writes `CUSTOM_PRE_BROADCAST` section.

### B.6 `writeEncodingConfiguration`
No-op if `!share_encoders`. Otherwise, collects the distinct `EncodingFormat`s needed across `station->mounts`, `station->remotes`, and (if `enable_hls`) `station->hls_streams`, keyed by `getVariableName('radio')` to dedupe identical encoder configs, and emits one `{var} = ffmpeg.encode.audio(%ffmpeg({audio_string}), radio)` per distinct encoder. This produces a single shared encoded stream per unique format/bitrate/profile combination, which is then reused (via `%format.copy`/`.drop`) by every output that needs that exact encoding, instead of each output running its own independent encoder instance.

Exact per-format `%audio(...)` strings (from `getFfmpegAudioString`, used both here and by every non-shared-encoder output):
- **AAC**: `%audio(codec="libfdk_aac", samplerate=44100, channels=2, b="{bitrate}k", profile="{profile}", afterburner={0|1})` — `afterburner=1` if `bitrate>=160` else `0`; `profile` from the HLS sub-profile name if set, else `HlsStreamProfiles::default()`.
- **Ogg**: `%audio(codec="libvorbis", samplerate=48000, b="{bitrate}k", channels=2)`.
- **Opus**: `%audio(codec="libopus", samplerate=48000, b="{bitrate}k", vbr="constrained", application="audio", channels=2, compression_level=10, cutoff=20000)`.
- **FLAC**: `%audio(codec="flac", channels=2, ar=48000)` (no bitrate — lossless).
- **MP3**: `%audio(codec="libmp3lame", ac=2, ar=44100, b="{bitrate}k")`.
- Any other format throws `RuntimeException('Unsupported stream format: ...')` at generation time (should never occur given the closed `StreamFormats` enum).

### B.7 `writeLocalBroadcastConfiguration`
No-op if `station->frontend_type === Remote`. Otherwise iterates `station->mounts` in order, and for each mount with a non-null `getOutputtableSource()`, emits one output line via `getOutputString(..., idPrefix="local_", id=1-based index)` (see B.14).

### B.8 `writeHlsBroadcastConfiguration`
No-op if `!enable_hls`, and returns before emitting anything if the computed `hlsStreams` list ends up empty (e.g. all HLS streams somehow lack an encoding format).
- Builds `hls_streams = [("{var_name}", %ffmpeg(format="mpegts", {audio_or_copy_string})), ...]` — one tuple per configured HLS stream, `var_name` = `cleanUpVarName(hlsStream->name)`.
  - If `share_encoders`: for each HLS stream's tuple, the *inner* ffmpeg list contains one entry **per HLS stream total** — `%{that_stream's_shared_encoder_var}.copy` for the stream matching the current tuple's own encoding, and `%{other_stream's_shared_encoder_var}.drop` for every other stream (i.e. every tuple explicitly lists copy-self/drop-others across all streams — this is how Liquidsoap's `output.file.hls` multi-bitrate ladder shares one physical encoder pass per unique format while still presenting N named renditions).
  - If not sharing: each tuple's ffmpeg body is just that stream's own `getFfmpegAudioString(...)` (independent encoder).
- If `share_encoders`, additionally builds an aggregate `hls_radio` source: for the *first* HLS stream, extracts `{audio, metadata=hls_m, track_marks=hls_tm} = source.tracks({encoder_var})`; for each subsequent stream, extracts just `{audio}`; then `hls_radio = source({hls_audio_1=hls_audio_1, ..., metadata=hls_m, track_marks=hls_tm})`. The variable fed to `output.file.hls` is `hls_radio` in this case, or plain `radio` otherwise.
- Final block:
  ```
  def hls_segment_name(seg_meta) =
      seg_timestamp = int_of_float(time())
      seg_duration = {hls_segment_length}
      "#{seg_meta.stream_name}_#{seg_duration}_#{seg_timestamp}_#{seg_meta.position}.#{seg_meta.extname}"
  end

  output.file.hls(playlist="live.m3u8",
      segment_duration={hls_segment_length}.0,
      segments={hls_segments_in_playlist},
      segments_overhead={hls_segments_overhead},
      segment_name=hls_segment_name,
      persist_at="{config_dir}/hls.config",
      temp_dir="#{settings.azuracast.temp_path()}",
      "{hls_base_dir}",
      hls_streams,
      {radio | hls_radio}
  )
  ```
  Segment filenames follow the exact pattern `{stream_name}_{segment_duration}_{unix_timestamp}_{position}.{ext}`.

### B.9 `writeRemoteBroadcastConfiguration`
Iterates `station->remotes`, same one-line-per-remote pattern as B.7 but `idPrefix="relay_"`.

### B.10 `writePostBroadcastConfiguration`
Only writes the `CUSTOM_BOTTOM` custom-config section — nothing else.

### B.11 `writeCustomConfigurationSection($event, $sectionName)` (helper, called from multiple methods above)
- If the config is being generated **for editing** (`$event->isForEditing()`, e.g. the "view/edit raw Liquidsoap config" UI feature), emits a divider line `{chr(7)}{sectionName}{chr(7)}` (a literal ASCII BEL character used as an unambiguous machine-parseable delimiter) and returns — the actual custom text is *not* embedded in edit mode (presumably the UI re-splices it back in around these markers).
- Otherwise (real generation for the running Liquidsoap process): if the instance-wide setting `enable_liquidsoap_editing` is false, does nothing at all (custom sections are silently dropped from the real running config even if populated in the DB — this is a safety/lockdown toggle). If enabled and the named section's stored text is non-empty, emits:
  ```
  # Custom Configuration (Specified in Station Profile)
  # startcustomconfig({sectionName})
  {raw custom text, verbatim}
  # endcustomconfig({sectionName})
  ```

### B.12 `getScheduledPlaylistPlayTime` (helper)
Converts a `StationSchedule` row into a Liquidsoap time-predicate string:
- **Overnight window** (`start_time > end_time`): splits into two half-day predicates joined by `or`: `"{start}-23h59m59s"` and `"00h00m-{end}"`. If the schedule is restricted to specific days of week (`count(days) < 7`), each half gets its own day-of-week predicate ANDed in — critically, the *day set for the second half is shifted forward by one day* relative to the first (since the overnight window's tail falls on the calendar day after `days` for its first half), with day `7` (Sunday) mapped to Liquidsoap's `"0w"` day token and day `n` otherwise mapped to `"{n}w"`, wrapping `day+1` back to `1` when it exceeds `7`.
- **Same-day window**: `"{start}"` alone if `start_time === end_time` (single instantaneous trigger, e.g. used by `OncePerHour`'s minute check), else `"{start}-{end}"`. If restricted to specific days, ANDs in an `(Nw or Mw or ...)` day predicate (no day-shifting needed here since it's not overnight).
- **Date-range boundaries** (`start_date`/`end_date` set): generates a dedicated named function `schedule_{schedule_id}_date_range()` that computes `current_time = time()` and compares against Unix-timestamp literals for the range boundaries (`start_date` at `00:00:00` station-local, `end_date` at `23:59:59` station-local, both computed at config-generation time and baked in as **fixed timestamps**, not re-evaluated relative to "today" — meaning a regenerated config is required whenever these boundaries need to move, and DST transitions are resolved once, at generation time, using the station's timezone object). The final time predicate becomes `schedule_{id}_date_range() and ({original playtime predicate})`.
- Time-of-day formatting (`formatTimeCode`): input is an integer in `HMM`/`HHMM`-style encoding (e.g. `930` = 9:30, `1730` = 17:30); output is `"{hours}h{minutes}m"` where `hours = floor(code/100)`, `minutes = code % 100` (no zero-padding, e.g. `9h5m` for `905`).

### B.13 `shouldWritePlaylist` (helper)
Returns `true` (write this playlist into the `.liq` file) if **any** of:
- `write_playlists_to_liquidsoap` is true (station-wide override — always write everything), or
- `use_manual_autodj` is true (manual mode needs every playlist literally present since there's no DB-driven AutoDJ loop to fall back on), or
- the playlist's `source !== Songs` (i.e. any remote-URL playlist is always written — only local "Songs" playlists can be optimized away), or
- the playlist has any of `backendInterruptOtherSongs()`, `backendPlaySingleTrack()`, `backendMerge()`, or `type === Advanced` (these behaviors can only be expressed by an actual `.liq`-level construct, not by the DB-driven AutoDJ queue).

Otherwise `false` — meaning a plain "Songs" playlist with none of those special options, under default settings, is deliberately **not** written into the generated Liquidsoap file at all; it's expected to be selected purely through the AutoDJ database queue/`nextsong` callback mechanism instead (see D.2/C.3). This is an important optimization the Rust engine must replicate exactly, since writing such playlists anyway would double-count them in rotation.

### B.14 `getOutputString` (helper, used by B.7/B.9 and indirectly documents the HLS "non-shared" fallback path in B.8)
Builds one `output.icecast(...)` or `output.shoutcast(...)` call (selection: `source->isShoutcast`):
- Audio param: `%ffmpeg(format="{container}", %{shared_encoder_var}.copy)` if `share_encoders`, else `%ffmpeg(format="{container}", {full audio string})`.
- `id="{idPrefix}{index}"`.
- `host`, `port` (cast to int).
- `user` only if `username` non-empty.
- `password` — for Shoutcast adapter type, the literal source-index `:#{id}` is appended to the password (Shoutcast DNAS2's convention for multi-mount auth).
- Mount handling: for `Icy` protocol, `icy_id={id}` is added *only if* a mount point string is present (Icy/Shoutcast doesn't use path-style mounts the same way); for non-Icy protocols, `mount="{mount or '/'}"` is always set (defaults to `/` if empty).
- `name`, and (`description` only if not Shoutcast — Shoutcast doesn't support a separate description field), `genre`, and `url` (only if non-empty).
- `public={true|false}`.
- `encoding="{charset}"`.
- `transport=https_transport` only for `Https` protocol.
- `send_icy_metadata=` only emitted if the format's `sendIcyMetadata()` returns non-null (currently only FLAC forces `true`; all other formats omit the param entirely, letting Liquidsoap use its own default).
- Final positional source arg: the shared encoder var name (if sharing) or plain `radio` (if not).

### B.15 String/value helper semantics (exact, must be bit-for-bit reproduced)
- `toFloat($n, $decimals=2)`: `number_format((float)$n, $decimals, '.', '')` — always fixed-point, never scientific notation, always a specific number of decimal digits (default 2), no thousands separators.
- `toRawString($value)`: wraps a string in Liquidsoap's `{delimiter|...|delimiter}` raw-string syntax using a randomly-generated 5-lowercase-letter delimiter (`str_` + random), so that arbitrary station-supplied text (titles, URLs, passwords, custom text) can never break out of the string literal via embedded quotes — any occurrence of the *exact* close-delimiter sequence inside the value is stripped (a purely defensive collision-avoidance measure, not general escaping) before embedding. Every per-station free-text field (name, description, genre, url, passwords, API key/URL, timezone, fallback path, temp path, live-broadcast text, custom metadata command strings, remote URLs, Stereo Tool paths/keys) is emitted through this helper rather than naive double-quoting.
- `cleanUpVarName($str)`: strips tags, collapses whitespace and a blocklist of punctuation (`"*/:<>?'|`) to single spaces, lowercases, round-trips through HTML-entity encode/decode to normalize special characters, strips any resulting numeric/named HTML entities down to their base letter, URL-encodes, replaces spaces with underscores, and finally strips `%`, converts `-`/`.` to `_`. This produces a Liquidsoap-identifier-safe token from arbitrary user-supplied names (playlist names, HLS stream names, mount var names for shared encoders).
- `getPlaylistVariableName`: `cleanUpVarName('playlist_' . StationPlaylist::generateShortName($playlist->name))`.
- `annotateValue`/`annotateArray`: used to build the `annotate:key="val",...:path` prefix consumed by `request.create` on the Liquidsoap side (see C.3/D.1/D.7). Only keys present in `AnnotateNextSong::ALLOWED_ANNOTATIONS` survive filtering; `title`/`artist` are always force-stringified (`ALWAYS_STRING_ANNOTATIONS`) while other scalar types get type-appropriate formatting (`true`/`false` passed through literally, numeric non-int values through `toFloat`, everything else as a string), and embedded `"`, newlines, tabs, carriage returns are stripped/escaped (`"`→`\"`, others→removed) since these values are being embedded inside a single Liquidsoap annotation string, not wrapped via `toRawString`'s delimiter trick.

---

## C. Runtime Library Behavior (`azuracast.liq` + `crossfade.liq` + `utilities.liq`)

### C.1 Settings registry
All station-tunable values ConfigWriter writes live under the `settings.azuracast.*` namespace, declared via `settings.make(description=..., default)` in `azuracast.liq`. Defaults (used whenever ConfigWriter doesn't override them): `liquidsoap_api_port=8004`, `api_url=""`, `api_key=""`, `media_path="api"`, `fallback_path="/usr/local/share/icecast/web/error.mp3"`, `temp_path="/tmp"`, `compute_autocue=false`, `default_fade=0.0`, `default_cross=0.0`, `enable_crossfade=false`, `crossfade_type="normal"`, `crossfade_smart_high=-15.0`, `crossfade_smart_medium=-32.0`, `crossfade_smart_margin=8.0`, `request_timeout=20.0`, `http_timeout=10.0`, `live_broadcast_text="Live Broadcast"`, `apply_amplify=true`. Global mutable state refs: `autodj_is_loading=true`, `autodj_ping_attempts=0`, `last_authenticated_dj=""`, `last_authenticated_dj_name=""`, `live_dj=""`, `live_dj_name=""`, `to_live=false`, `live_enabled=false`, `last_title=""`, `last_artist=""`.

### C.2 HTTP API server & shared call helper
- `azuracast.start_http_api(port=null)` → `server.harbor(port = port ?? liquidsoap_api_port())` — this is Liquidsoap's **own inbound** HTTP server (distinct from the outbound calls to AzuraCast), used for the `radio skip` and `custom_metadata.insert` server/telnet-style commands registered by `utilities.liq` (C.9). Every request is logged (method, path, status, duration) via a registered middleware `azuracast.http_api_log_requests`, and gated by `azuracast.http_api_check_token` which rejects (`401`) any request whose `x-liquidsoap-api-key` header doesn't match `settings.azuracast.api_key()`.
- `azuracast.api_call(timeout=null, endpoint_url, payload)` — the single outbound-call primitive used by every callback into AzuraCast's PHP API. Always `http.post` to `"{api_url}/{endpoint_url}"`, headers `Content-Type: application/json`, `User-Agent: Liquidsoap AzuraCast`, `X-Liquidsoap-Api-Key: {api_key}`, body = `payload` (caller-supplied, already-JSON-stringified string, or empty string for no-payload calls). `timeout` defaults to `settings.azuracast.http_timeout()` (10s) if not explicitly passed. On non-200 response: returns `null` (**not** an error — the caller must treat `null` as "no data" uniformly with actual transport failures). On thrown error (network failure, timeout, etc., caught via `try/catch`): logs and returns `null`. On success: returns the raw response body as a string (`"#{response}"}`, i.e. NOT pre-parsed — callers each do their own `json.parse` with an explicit expected shape).

### C.3 AutoDJ next-song polling & retry/failure behavior
- `azuracast.autodj_next_song()`: calls `azuracast.api_call("nextsong", "")` (empty-string payload, i.e. an empty POST body). If a non-null response comes back, `json.parse`s it against the strict shape `{uri: string}` and returns `request.create(uri)`. If the API call returned `null` (any failure, including a non-200 from an empty-queue `RuntimeException` on the PHP side — see D.2) or the JSON parse throws, returns `null`.
- `azuracast.enable_autodj(s)` wraps the incoming source `s` (the full playlist/schedule graph built in B.2) as follows:
  - `dynamic = request.dynamic(id="next_song", timeout=settings.azuracast.request_timeout(), retry_delay=10., azuracast.autodj_next_song)` — Liquidsoap's built-in dynamic-request source. `timeout` (default 20s) bounds how long Liquidsoap will wait for a *created* request to resolve/become playable; `retry_delay=10.` (hardcoded, not station-configurable) is how long `request.dynamic` waits before calling `azuracast.autodj_next_song` again after the function returns `null` (empty queue, API error, parse error) or after a created request fails to resolve. Retry/failure timing beyond this (e.g. exactly how Liquidsoap's own `request.dynamic` internals behave when a *resolved* request subsequently fails to *play*) is delegated to Liquidsoap core and not reimplemented in AzuraCast's script layer — only the `retry_delay`/`timeout` knobs are AzuraCast-specific.
  - `dynamic_startup = fallback(id="dynamic_startup", track_sensitive=false, [dynamic, source.available(blank(id="autodj_startup_blank", duration=120.), predicate.activates({azuracast.autodj_is_loading()}))])` — while the AutoDJ is still "loading" (see below), a 120-second-duration `blank()` silence source is available as filler underneath `dynamic`; once loading finishes (successfully or by giving up), the blank filler becomes permanently unavailable (`predicate.activates` gates its *availability*, not just preference, on the ref's live value).
  - `s = fallback(id="autodj_fallback", track_sensitive=true, [dynamic_startup, s])` — the AutoDJ dynamic source takes priority over the original `s` chain whenever ready.
  - A recurrent background poll is started: `thread.run.recurrent(delay=0.25, {azuracast.wait_for_next_song(dynamic)})`.
  - `azuracast.wait_for_next_song(autodj)`: on every invocation, increments `autodj_ping_attempts` first. If `source.is_ready(autodj)`: logs "AutoDJ is ready!", sets `autodj_is_loading := false`, returns `-1.0` (a negative return value stops the recurrent thread permanently — this poll only ever runs once total per Liquidsoap process lifetime, during startup). Else if `autodj_ping_attempts() > 200`: logs an error ("AutoDJ could not be initialized within the specified timeout"), sets `autodj_is_loading := false` anyway (giving up, not retrying further), returns `-1.0` (stop polling). Else: returns `0.5` (schedule next poll in 0.5s).
  - **Exact cadence**: first check fires at `t≈0.25s` after startup (the `thread.run.recurrent(delay=0.25, ...)` initial delay); every subsequent check is spaced `0.5s` apart (the callback's own return value, once it starts returning `0.5` instead of the initial schedule); the loop terminates after either becoming ready, or after attempt #201 (i.e. roughly `0.25 + 200×0.5 ≈ 100.25` seconds of unready polling) — at which point the `autodj_startup_blank` filler is permanently deactivated regardless of whether AutoDJ ever became ready, meaning a station whose AutoDJ never initializes will fall silent (no blank filler) rather than loop the blank source forever.

### C.4 DJ authentication / connect / disconnect sequencing
- `azuracast.dj_auth(auth_info)` — passed as `harbor`'s `auth=` callback (invoked by Liquidsoap on every incoming source-client connection attempt, receiving whatever fields the client's ICY/source-protocol auth handshake supplies, typically at least `user`/`password`). Calls `azuracast.api_call(timeout=5.0, "auth", json.stringify(auth_info))` (5-second timeout, distinct from the general `http_timeout`). On success, parses `{allow: bool, username: string?, display_name: string?}`; if `allow` is true, sets `last_authenticated_dj := username ?? ''` and `last_authenticated_dj_name := display_name ?? live_broadcast_text()`, then returns `true`; if `allow` is false, or the API call/JSON-parse failed, returns `false` (source connection rejected by harbor). Note: `last_authenticated_dj`/`_name` are set as a **side effect of authentication succeeding**, not of the connection actually completing — if two auth attempts race, the last one to set these refs "wins" regardless of which connection subsequently triggers `on_connect`, since there's no session-correlation id tying a specific auth call to a specific subsequent connect callback.
- `azuracast.live_connected(header)` (harbor `on_connect`, async/`synchronous=false`): reads current `last_authenticated_dj`/`_name` refs, logs them plus the raw connection `header`, sets `live_enabled := true`, `live_dj := {that dj}`, `live_dj_name := {that name}`, then asynchronously (`thread.run(fast=false, f)`) POSTs `djon` with `{user = live_dj()}`.
- `azuracast.live_disconnected()` (harbor `on_disconnect`, async): sets `live_enabled := false` **synchronously/immediately** (this flag flip happens before the async thread is even spawned), then spawns an async thread that POSTs `djoff` with `{user = live_dj()}`, and only *after* that HTTP call completes, resets `live_dj := ""` and `live_dj_name := ""`. Practical implication: `live_enabled` goes false immediately on disconnect, but `live_dj`/`live_dj_name` remain populated with the outgoing DJ's identity for up to the 5-second `djoff` call's duration (or until it errors out) — any code reading `live_dj()` during that window still sees the just-disconnected DJ, not an empty string.
- There is no explicit reconnection cooldown, debounce, or lock in the `.liq` layer itself for DJ sessions — the only cooldown mechanism in the whole system is the PHP-side `disconnect_deactivate_streamer` field (A.6), which is applied only when an operator/scheduler-driven **forced** disconnect happens (`Liquidsoap::disconnectStreamer()`), not on a DJ's own voluntary disconnect. A forced disconnect also issues the Liquidsoap-side `input_streamer.stop` telnet/HTTP command, which is expected to trigger harbor's normal `on_disconnect` → `azuracast.live_disconnected()` path like any other disconnect.

### C.5 Crossfade — dB-curve math and dispatch
`azuracast.apply_crossfade(s) = cross(duration=settings.azuracast.default_cross(), azuracast.live_aware_crossfade_impl, s)` — Liquidsoap's `cross()` operator with a custom transition callback.

`azuracast.live_aware_crossfade_impl(old, new)` dispatch order (first matching branch wins):
1. **To-live special case**: if `azuracast.to_live()` is true (i.e. the "check_live" mechanism in B.4 detected the harbor live source just became ready and set this flag), ignores crossfade settings entirely and does a hard sequence: `sequence([fade.out(duration=default_fade(), old.source), fade.in(duration=default_fade(), new.source)])` — old fades out completely, *then* new fades in (not overlapped) — "almost no fade-in" per the source comment, since these are typically back-to-back with the DJ's own levels.
2. Else if crossfade is enabled (`settings.azuracast.enable_crossfade()`):
   - If `crossfade_type() == "smart"`: delegates to `cross.smart(old, new, high=crossfade_smart_high(), medium=crossfade_smart_medium(), margin=crossfade_smart_margin(), fade_in=default_fade(), fade_out=default_fade())` (see below for the exact branch logic).
   - Else (`"normal"`): `cross.simple(old.source, new.source, fade_in=default_fade(), fade_out=default_fade())` (a plain, unconditional crossfade — no dB analysis at all).
3. Else (crossfade disabled entirely): "beautiful add" — `add(normalize=false, [fade.in(initial_metadata=new.metadata, duration=default_fade(), new.source), fade.out(initial_metadata=old.metadata, duration=default_fade(), old.source)])` — i.e. even with crossfading *disabled*, a fade-in/fade-out `default_fade()`-duration overlap is still applied (this is NOT a hard cut — `enable_crossfade=false` only removes the dB-aware/simple crossfade logic, not fading itself, since `default_fade` — the plain fade duration — is independent of `default_cross`/`enable_crossfade`).

Every branch logs to label `azuracast.crossfade` at `log.info` level, and always logs each non-cover metadata key/value pair of `old`/`new` (via `metadata.cover.remove`) before making the decision, regardless of which branch is taken.

**`cross.smart(id, fade_in=3., fade_out=3., default=sequence, high=-15., medium=-32., margin=4., a, b)`** (defined in `crossfade.liq`; note its *own* internal defaults for `fade_in`/`fade_out`/`high`/`medium`/`margin` differ from `azuracast.liq`'s `settings.azuracast.*` defaults, but `azuracast.live_aware_crossfade_impl` always passes explicit values for all five, so `crossfade.liq`'s built-in defaults are never actually reached in the AzuraCast integration — they'd only matter if `cross.smart` were called directly from custom-injected `.liq` code without those params). Exact branch logic, evaluated top-to-bottom (first true condition wins):
1. `a.db_level <= medium AND b.db_level <= medium AND abs(a.db_level - b.db_level) <= margin` → **full crossfade**: `add(normalize=false, [fade.in(type="sin", duration=fade_in, b.source), fade.out(type="sin", duration=fade_out, a.source)])` (both tracks are quiet/similar enough to overlap safely; sine-curve fade shape).
2. Else if `b.db_level >= a.db_level + margin AND a.db_level >= medium AND b.db_level <= high` → **fade-out only**: `add(normalize=false, [b.source, fade.out(type="sin", duration=fade_out, a.source)])` (new track is significantly louder than old, but old isn't near-silent and new isn't blastingly loud — only fade the outgoing track, let the new one enter at full volume immediately).
3. Else if `a.db_level >= b.db_level + margin AND b.db_level >= medium AND a.db_level <= high` → **fade-in only** (mirror of #2): `add(normalize=false, [fade.in(type="sin", duration=fade_in, b.source), a.source])`.
4. Else if `b.db_level >= a.db_level + margin AND a.db_level <= medium AND b.db_level <= high` → **no fade at all, straight cut-in**: `add(normalize=false, [b.source, a.source])` (old is already very quiet — don't bother fading since there's essentially nothing there to fade).
5. Else (both tracks too loud to overlap nicely, or the gap between them is too extreme to mask either one without an audible artifact) → **falls through to `default`**, which in the AzuraCast integration is `cross.smart`'s own built-in default: `fun(a,b) -> sequence([a,b])` (hard sequential cut, no overlap whatsoever — the source code even notes in a comment that a jingle would be a better transition here, but none is implemented).

All five branches log a human-readable rationale string at `cross_id`-labeled level-3 logging (`cross_id` defaults to `"crossfade"` if not overridden) before executing.

### C.6 Feedback API (now-playing push to AzuraCast)
`azuracast.send_feedback(m)`: skipped entirely if `m["is_error_file"] == "true"` (the fallback/error jingle's own synthetic metadata tag — see C.8 — is deliberately never reported as now-playing). Otherwise, only sends anything if `m["title"]` or `m["artist"]` actually **differs** from the last-sent values (`last_title`/`last_artist` refs — a dedupe guard to avoid redundant callbacks on metadata re-emission that doesn't actually represent a track change), updating those refs immediately when it does differ. The payload sent is filtered down to exactly these keys (if present in the metadata): `song_id`, `media_id`, `playlist_id`, `sq_id`, `artist`, `title` (via `list.assoc.filter`, cover-art stripped first via `metadata.cover.remove`), JSON-stringified compactly (`compact=true`) and POSTed to the `feedback` endpoint with default (10s) timeout.

### C.7 Jingle-mode metadata suppression (two distinct mechanisms — do not conflate)
1. **Playlist-level, source-stripping** (`azuracast.utilities.drop_metadata`, applied in `ConfigWriter::writePlaylistConfiguration` when `playlist->is_jingle`): rebuilds the source's track stream with its `metadata` field completely discarded (`let {metadata=_, ...tracks} = source.tracks(s); source(id=..., tracks)`) — this only applies to playlists that are written **directly** into the `.liq` file as literal `playlist()`/`merge_tracks()` objects (schedule-driven jingle playlists), and produces genuinely empty/no metadata for those tracks at the source level, permanently, regardless of any other in-flight metadata state.
2. **Request-level, "replay previous" suppression** (`azuracast.handle_jingle_mode`, applied globally to `radio` at the very end of `writePreBroadcastConfiguration`, i.e. after live/fallback merging): maintains a `last_metadata` ref (initially `[]`). Implemented as `metadata.map(update=false, strip=true, handle_jingle_mode, s)`. Per-track logic: if the **incoming** track's metadata has `jingle_mode == "true"` (set by `Annotations::annotatePlaylist` in PHP whenever the queued track's playlist has `is_jingle=true` and the track is going through the normal AutoDJ **database** queue — see A.7/D.1), the function returns `last_metadata()` — i.e. it substitutes in whatever metadata was flowing *before* this jingle track, so listeners' "now playing" display doesn't change at all while the jingle plays (the jingle plays audibly but is invisible in metadata). Otherwise, it stores the current track's metadata into `last_metadata` and passes it through unchanged (this is the mechanism by which the "previous" metadata is captured for the *next* jingle to fall back to).
   - Net effect: a jingle played through the DB-driven AutoDJ path is audible but metadata-invisible (masquerades as a continuation of the prior track); a jingle played through a directly-scheduled `.liq` playlist object has genuinely blank metadata (shows as "nothing playing" rather than masquerading as the previous track). These are deliberately different behaviors for two different jingle-authoring paths and the Rust engine must preserve the distinction.

### C.8 Fallback / error-source behavior
`azuracast.add_fallback(s)`: builds a `single(id="error_jingle", "annotate:liq_disable_autocue=\"true\":{fallback_path}")` request for the configured fallback/error audio file (autocue explicitly disabled on this single request via the `liq_disable_autocue` annotation, so the error file is never subject to autocue-based cue/fade adjustment), tags its metadata with `is_error_file="true"` (via `metadata.map`), then wraps: `s = fallback(id="safe_fallback", track_sensitive=false, [s, error_file])`. Because `track_sensitive=false`, this fallback can interrupt `s` **mid-track** the instant `s` becomes unavailable (rather than waiting for the current track to end) — this is the true last-resort safety net: if literally every upstream source (harbor live, AutoDJ dynamic request, all playlists/schedule switches, remote-URL fallbacks, request queues) is simultaneously unavailable, Liquidsoap falls back to looping the single error/fallback audio file, tagged so `send_feedback` (C.6) knows to suppress it from being reported as now-playing.

Given the full chain built across B.2–B.5, the practical fallback ordering (highest to lowest priority, first-ready-wins per `fallback()`'s semantics) ends up being, outermost to innermost: remote-URL schedule overrides → interrupting request queue → live DJ harbor input → regular request queue → remote-URL station-wide fallback (if configured) → AutoDJ dynamic-request source (with its 120s startup blank filler) → the standard/scheduled/special playlist rotation graph from B.2 → (finally) the `safe_fallback` error/jingle file from `add_fallback`, which cannot itself fail (a local file loop).

### C.9 Utility telnet/HTTP-registered commands (`utilities.liq`)
- `azuracast.utilities.drop_metadata(id=null, s)`: see C.7 #1.
- `azuracast.utilities.add_skip_command(s)`: registers server command `radio skip` (namespace `"radio"`, usage `"skip"`) that calls `source.skip(s)` and returns the literal string `"Done!"`. This is the mechanism behind ConfigWriter's `azuracast.utilities.add_skip_command(radio)` call in B.3, allowing an operator/external tool to force-skip the current track over Liquidsoap's HTTP/telnet control interface.
- `azuracast.utilities.add_custom_metadata_command(id=null, s)`: registers server command `{id} insert` (default namespace `"custom_metadata"`, usage `insert key1="val1",key2="val2",..`) that parses the supplied string via `string.annotate.parse("{input}:")` (reusing Liquidsoap's own annotation-string grammar to parse a comma-separated `key="val"` list with no following path) and, if any metadata was successfully parsed, calls `source.methods(s).insert_metadata(meta)` and returns `"Done"`; on empty/unparseable input, returns the literal string `"Syntax error or no metadata given. Use key1=\"val1\",key2=\"val2\",.."` without touching the source. This backs `ConfigWriter`'s `azuracast.utilities.add_custom_metadata_command(id="custom_metadata", radio)` call in B.5.

### C.10 Autocue integration & the `media:` protocol
- `azuracast.media_protocol(rlog, maxtime, arg)` — registered as Liquidsoap protocol `media:` (`protocol.add("media", ..., syntax="media:uri")`). If `settings.azuracast.media_path() == "api"` (non-local storage backend), computes a millisecond timeout as `1000. * (maxtime - time())` and calls `azuracast.api_call(timeout=..., "cp", json.stringify({uri: arg}))`; on success, parses `{uri: string, isTemp: bool}` and returns `"tmp:{uri}"` if `isTemp` else the bare `uri` string (feeding into Liquidsoap's own `tmp:` protocol semantics for a file that should be cleaned up after use). On failure (null response or JSON parse error), returns `null` (request resolution fails). If media storage **is** local, simply returns `"{media_storage_dir}/{arg}"` directly, with no HTTP round-trip at all.
- `azuracast.autocue(request_metadata, file_metadata, filename)` — registered as the preferred autocue provider (`autocue.register(name="azuracast", ...)`, `settings.autocue.preferred := "azuracast"`, `enable_autocue_metadata()` also called unconditionally). Three-way branch:
  1. If `request_metadata["azuracast_autocue"] == "true"` (set by PHP when either real stored autocue metadata or cache-derived autocue values are available — see D.1/`Annotations::processAutocueAnnotations`): builds `{cue_in, cue_out, fade_in, fade_out}` (all `float_of_string`'d from the corresponding `autocue_*` annotation keys, which are always present together when this flag is true) plus **optionally** `start_next` (only if `autocue_start_next` key is present) and **optionally** `amplify` (only if `liq_amplify` key is present) — spread in via record extension syntax.
  2. Else if `settings.azuracast.compute_autocue()` is true (station opted into on-the-fly autocue computation): calls Liquidsoap's own `autocue.internal.implementation(...)` to compute cue points from the audio itself, then — if `request_metadata["azuracast_cache_key"]` is non-empty — asynchronously (`thread.run(fast=false, ...)`) POSTs the computed result back to the `savecache` endpoint (`{cache_key, data=autocue_result}`) so future requests for the same media can skip recomputation via mechanism #1 above (this is the write side of the "azuracast_cache_key"/`SaveCacheCommand` round trip — see D.7).
  3. Else: returns `null` (no autocue data at all — Liquidsoap falls back to whatever default cue behavior it has without autocue).

### C.11 Amplify & debug logging
- `azuracast.apply_amplify(s)`: `amplify(1., s)` wrapped as a typed `source` if `settings.azuracast.apply_amplify()` is true (the default, and — per B.1 — never actually toggled by ConfigWriter itself), else passthrough `s` unchanged. Note `amplify(1., s)` at a literal gain of `1.` is a no-op numerically by itself — the actual amplification amount comes from per-track `liq_amplify` metadata (set from the `Meta::AMPLIFY` media field via `Annotations::processAutocueAnnotations`, formatted as e.g. `"3.2 dB"`), which Liquidsoap's `amplify()` operator reads from stream metadata at playback time; this function's only real job is deciding whether the `amplify()` operator is in the signal chain **at all**.
- `azuracast.log_meta(m)`: debug-only tap registered in B.3. Logs every non-cover metadata key/value at level 4. Computes a "now playing" display string as `"{artist} - {title}"`, except if `artist` is empty and `title` contains `" - "`, it instead splits `title` on the first `" - "` and treats the two halves as artist/title for display purposes only (does not alter the actual metadata). Separately logs a filtered subset (`duration`, `media_id`, `replaygain_track_gain`, `replaygain_reference_loudness`, plus anything prefixed `azuracast_` or `liq_`) at level 3, followed by the "Now playing" line at level 3.

---

## D. Callback HTTP Contracts

### D.0 Common transport/envelope (applies to all seven commands)
- **Route**: `GET|POST /api/internal/{station_id}/liquidsoap/{action}` (Slim route name `api:internal:liquidsoap`, defined in `backend/config/routes/api_internal.php`), where `{action}` is one of the `LiquidsoapCommands` enum values: `cp`, `auth`, `djon`, `djoff`, `feedback`, `nextsong`, `savecache`.
- **Network-level gate**: the route carries `Middleware\RequireInternalConnection`, which rejects (throws `PermissionDeniedException`) any request where the server-param `IS_INTERNAL` is not truthy — i.e. this endpoint is only reachable from whatever internal-network path the reverse proxy marks as internal (e.g. a Docker-internal listener/port), regardless of API-key correctness. The Rust engine must originate these calls from that same internal path/port, not the public-facing API.
- **Auth mechanism** (inside `LiquidsoapAction::__invoke`): reads header `X-Liquidsoap-Api-Key`. If the requester's ACL does **not** already grant `StationPermissions::View` on the station (i.e. an unauthenticated/non-session request, which is the normal case for the Liquidsoap process itself), the header value must `hash_equals`-match the station's `adapter_api_key` (`Station::validateAdapterApiKey`) or the call throws `RuntimeException('Invalid API key.')` → HTTP 400; on match, the request is flagged `$asAutoDj = true`. If the requester **does** already have session/ACL-based View access (e.g. a logged-in admin hitting this internal endpoint manually, such as for debug tooling), `$asAutoDj` is instead set to whether a *valid* adapter API key was *also* supplied (`!empty($authKey) && validateAdapterApiKey($authKey)`) — meaning a plain logged-in admin without the adapter key gets `$asAutoDj=false`. This `$asAutoDj` boolean is passed through to every command's `doRun()` and several commands (`DjOnCommand`, `DjOffCommand`, `FeedbackCommand`, `SaveCacheCommand`) **no-op (return false) unless it's true** — i.e. these four commands functionally require the correct adapter API key regardless of ACL state; only `View`-ACL'd + adapter-key-present callers, or the Liquidsoap process itself, can actually trigger their real effects.
- **Request body**: `$payload = (array)$request->getParsedBody()`. On the Liquidsoap side, every call goes through `azuracast.api_call`, which always POSTs `Content-Type: application/json` with a JSON-stringified body (or a literal empty string `""` for `nextsong`, which has no request-side parameters).
- **Response**: on success, `$response->withJson($commandObj->run(...))` — HTTP 200, JSON body being whatever `doRun()` returned (array → JSON object; bool → JSON `true`/`false`, which Liquidsoap-side callers generally ignore or don't even parse for these). On **any** thrown exception anywhere in the command (`Throwable`), HTTP 400 with JSON body `{"message": "...", "file": "{basename}", "line": N}`; the Liquidsoap-side `azuracast.api_call` treats *any* non-200 status as equivalent to a hard failure and returns `null` — it never inspects the error body.
- **Command dispatch wrapper** (`AbstractCommand::run`): rejects (throws `LogicException`) if the station's `backend_type` isn't `BackendAdapters::Liquidsoap` (defense against this endpoint being hit for a non-Liquidsoap-backed station); pushes a logging processor that tags every log line for the duration of the call with `{station: {id, name}}`; logs a debug line naming the concrete command class plus `asAutoDj`/`payload`; delegates to the subclass's `doRun($station, $asAutoDj, $payload)`; pops the logging processor in a `finally` (always runs, even on exception).

### D.1 `NextSongCommand` — `nextsong`
- **Liquidsoap caller**: `azuracast.autodj_next_song()` (C.3), payload always `""` (empty body).
- **PHP behavior**: calls `Annotations::annotateNextSong($station, $asAutoDj)`, which:
  1. Calls `StationQueueRepository::getNextToSendToAutoDj($station)` — if this returns `null` (**AutoDJ queue is empty**), throws `RuntimeException('Queue is empty!')`, which propagates all the way out to `LiquidsoapAction`'s catch-all, producing an **HTTP 400** response. On the Liquidsoap side, this is indistinguishable from any other transport failure: `azuracast.api_call` sees non-200 and returns `null`, `azuracast.autodj_next_song` returns `null`, and `request.dynamic` retries after its `retry_delay=10.` seconds (C.3). **This is the exact, verified mechanism for "AutoDJ queue empty" behavior** — there is no special-case empty-queue payload; it's a generic HTTP error.
  2. Otherwise, builds an `AnnotateNextSong` event from the queue row (carrying station, queue, media, playlist, request, and the `asAutoDj` flag) and dispatches it through a chain of prioritized listeners (`Annotations` class itself, priorities 20→15→12→10→5→-10):
     - `annotateSongPath`(20): sets the request's path — `"media:" + ltrim(media->path, '/')` if there's a `StationMedia` row, else the queue row's `autodj_custom_uri` if set (custom/external URI override), else nothing (would later throw `'No valid path for song.'` if truly nothing was set).
     - `annotateForLiquidsoap`(15): if a media row exists and the backend is enabled, adds `title`, `artist`, `duration` (=`media->length`), `song_id`, `media_id`, `sq_id` (=queue row id), plus autocue annotations derived from the *media's own stored* `extra_metadata` (see below), plus any station custom fields for that media.
     - `addCachedAutocueData`(12): if a media row exists, computes `azuracast_cache_key` (from `AutoCueCache::getCacheKey($media)`) and adds autocue annotations derived from the **cache-stored** autocue data for that key (this can supplement/override values not present from the media's own stored metadata — both listeners call the same `processAutocueAnnotations` helper and merge via `addAnnotations`, later keys overwriting earlier ones for the same annotation name since `addAnnotations` does an `array_merge`).
     - `annotatePlaylist`(10): if a playlist is associated, adds `playlist_id`; if that playlist's `is_jingle` is true, adds `jingle_mode="true"` (this is the flag `azuracast.handle_jingle_mode` checks — C.7 #2).
     - `annotateRequest`(5): if a `StationRequest` (listener song request) is associated, adds `request_id`.
     - `postAnnotation`(-10): only if `asAutoDj` is true: marks the queue row `sent_to_autodj=true`, stamps `timestamp_cued=now()`, persists — i.e. **querying `nextsong` without the adapter API key does not consume/mark the queue entry**, only genuine AutoDJ (Liquidsoap-authenticated) calls do.
  3. `processAutocueAnnotations` (shared helper, used by both the "own metadata" and "cache" listeners) — exact rules: drops null values; if `cue_out` is negative, treats it as "seconds before the end" and recomputes it as `max(0, duration - abs(cue_out))` (dropping the key entirely if the result would be `0` or if `abs(cue_out) > duration`); drops `cue_out` if it exceeds `duration`; drops `cue_in` if it exceeds `duration`; if nothing is left after these prunings, returns `[]` (no autocue data at all, `azuracast_autocue` never gets set true); if `amplify` is present, formats it as `"{n} dB"` (appending `" dB"` unless already present) and — **if amplify was the *only* surviving annotation** — short-circuits and returns just `{'liq_amplify': ...}` (no `azuracast_autocue=true`, no cue/fade fields at all, since there's nothing else to justify a full autocue payload); otherwise defaults missing `cue_in`→`0.0` and `cue_out`→`duration`, forces `cue_in=0.0, cue_out=duration` if the stored `cue_out < cue_in` (sanity clamp), drops `start_next` if it falls outside `[cue_in, cue_out]`, defaults missing `fade_in`/`fade_out` to the station's `crossfade` value if crossfade is enabled else `0.0`, and finally returns the full `{azuracast_autocue: true, liq_amplify, autocue_cue_in, autocue_cue_out, autocue_fade_in, autocue_fade_out, autocue_start_next}` set (nulls allowed for `liq_amplify`/`autocue_start_next`, which get filtered out by `ConfigWriter::annotateArray`'s allowed-annotation + non-null filtering when the final string is built).
- **Response body**: `{"uri": "{annotate:key=\"val\",...:path | bare path}"}` — the exact string built by `AnnotateNextSong::buildAnnotations()`: if any annotations survived, `"annotate:{annotateArray output}:{songPath}"`, else just `{songPath}` bare.
- **Liquidsoap consumption**: parses `{uri: string}` and calls `request.create(uri)` — Liquidsoap's own annotation-string parser then extracts the `key="val"` pairs back out of the `annotate:...:path` syntax as the created request's metadata, which is what `azuracast.autocue`'s `request_metadata` parameter receives (C.10) and what `Annotations::annotatePlaylist`'s `jingle_mode` flag rides on into `azuracast.handle_jingle_mode` (C.7).

### D.2 `DjAuthCommand` — `auth`
- **Liquidsoap caller**: `azuracast.dj_auth(auth_info)` (harbor `auth=` callback), payload = `json.stringify(auth_info)` (whatever fields harbor's own auth handshake collects — practically `user`/`password` at minimum).
- **PHP behavior**: throws `RuntimeException('Streamers are disabled on this station.')` (→ HTTP 400 → Liquidsoap sees non-200 → `dj_auth` catches parse failure path and returns `false`, rejecting the connection) if `!station->enable_streamers`. Extracts credentials via `getCredentials($payload)`:
  - `user = payload['user']` (string-or-null), `pass = payload['password']` (string-or-null); throws `InvalidArgumentException` if `pass` is null/absent.
  - **Special case**: if `user` is null or literally `"source"`, and `pass` contains a `,` or `:` separator, it's re-split as `user, pass = explode(separator, pass, 2)` (checked in that order — `,` first, `:` second) — this supports legacy "combined user:pass in the password field" client behavior (e.g. some encoders that only expose a single password field for source auth).
  - Throws `InvalidArgumentException('No credentials provided!')` if `user` is still null after that.
  - **Source-password bypass**: if the resolved `user === 'source'`, and the station's `frontend_config->source_pw` is non-empty and exactly `strcmp`-equals the supplied password, immediately returns `{"allow": true, "username": "source"}` — **without** consulting the streamer repository at all (a station-wide "broadcast source" password, separate from any individual DJ account).
  - Otherwise, looks up `StationStreamerRepository::getStreamer($station, $user)` (implicitly `is_active=1` only — a deactivated/cooldown streamer, per C.4's note on `disconnect_deactivate_streamer`, will not be found here and thus auth fails as if the account didn't exist at all). If not found: `{"allow": false}`. If found: `{"allow": streamer->authenticate($pass) && Scheduler::canStreamerStreamNow($streamer), "username": streamer->streamer_username, "display_name": streamer->display_name}` — note `username`/`display_name` are returned **even when `allow` ends up false** (e.g. correct password but outside the streamer's scheduled window) — the Liquidsoap side only reads `username`/`display_name` when `allow` is true, so this is harmless, but worth noting for exactness.
  - `streamer->authenticate($pass)`: `password_verify($pass, streamer_password_hash)` (bcrypt/argon-style verification via a raw reflected private property, not a plain string compare).
  - `Scheduler::canStreamerStreamNow($streamer)`: `true` unconditionally if `streamer->enforce_schedule` is false; otherwise requires an active `StationSchedule` row (time/day-of-week/date-range match, evaluated in the station's timezone) among the streamer's own `schedule_items`.
- **Response body**: `{"allow": bool, "username"?: string, "display_name"?: string}`.

### D.3 `DjOnCommand` — `djon`
- **Liquidsoap caller**: `azuracast.live_connected` async thread, payload `{"user": live_dj()}`.
- **PHP behavior**: always logs a notice `'Received "DJ connected" ping from Liquidsoap.'` with `dj={user}`. If `!$asAutoDj` (i.e. adapter API key wasn't valid/supplied), returns `false` **without doing anything else** (no state change) — so a manually-triggered call (e.g. from an admin session without the adapter key) is a pure no-op beyond logging. If `$asAutoDj`, calls `StationStreamerRepository::onConnect($station, $user)`:
  - Ends **all** currently-active broadcast records for the station (`endAllActiveBroadcasts`) — a defensive cleanup in case a previous session wasn't cleanly closed.
  - Looks up the streamer by `$user` (`getStreamer`, active-only); if not found, returns `false` (station state is left untouched even though a connection notification was received — this can happen if, e.g., the `source` bypass path was used, since there's no real streamer row for `"source"`).
  - Otherwise sets `station->is_streamer_live = true`, `station->current_streamer = {that streamer}`, persists; creates a new `StationStreamerBroadcast` row (broadcast history entry) and persists/flushes.
- **Response body**: `true`/`false` (unused by the `.liq` caller, which doesn't inspect the response at all — `thread.run(fast=false, f)` discards `f`'s return value via `_ = azuracast.api_call(...)`).

### D.4 `DjOffCommand` — `djoff`
- **Liquidsoap caller**: `azuracast.live_disconnected` async thread, payload `{"user": live_dj()}` (note: the payload's `user` field is **not actually read** on the PHP side at all — see below).
- **PHP behavior**: always logs `'Received "DJ disconnected" ping from Liquidsoap.'` (no `dj` field logged here, unlike `DjOnCommand`). If `!$asAutoDj`, returns `false` (no-op). If `$asAutoDj`, calls `StationStreamerRepository::onDisconnect($station)` — takes **no username parameter at all**; it closes every currently-`getActiveBroadcasts($station)` row by stamping `timestampEnd = now()`, then unconditionally sets `station->is_streamer_live = false`, `station->current_streamer = null`, persists/flushes. Since it doesn't check *which* streamer disconnected, a `djoff` call always tears down whatever the station's current live state is, regardless of the `user` payload field's value.
- **Response body**: `true`/`false` (ignored by caller, same as `djon`).

### D.5 `FeedbackCommand` — `feedback`
- **Liquidsoap caller**: `azuracast.send_feedback(m)` (metadata `on_metadata` hook, see C.6), payload = compact JSON of the filtered `{song_id?, media_id?, playlist_id?, sq_id?, artist?, title?}` subset.
- **PHP behavior**: returns `false` immediately if `!$asAutoDj` (no-op for non-adapter-authenticated calls). Otherwise:
  - Coerces payload values: strings `"true"`/`"false"` → PHP bool, numeric strings → float, everything else passed through as-is (this is because Liquidsoap's `list.assoc`-derived JSON encoding stringifies everything uniformly, and PHP needs to undo that for correct typing before entity assignment).
  - Builds a `SongHistory` row via `getSongHistory($station, $payload)`:
    - If `media_id` is empty: requires at least one of `artist`/`title` non-empty (else throws `RuntimeException('No payload provided.')` → HTTP 400 → Liquidsoap-side caller ignores the failure, since it discards the return value with `_ = ...` — this is a **fire-and-forget** callback, errors are silently swallowed from Liquidsoap's perspective). Builds an ad-hoc `Song` from just `artist`/`title`; requires `SongHistoryRepository::isDifferentFromCurrentSong` to be true (dedupe against whatever's currently marked as playing) or throws `RuntimeException('Song is not different from current song.')`. Returns a bare `new SongHistory($station, $newSong)` (no media/queue linkage at all — this path is for tracks with no `StationMedia` row, e.g. purely external/manually-set metadata).
    - Else (media_id present): looks up the `StationMedia` row (throws if missing); requires it to be different from the current song (same dedupe check, same exception on failure); if `sq_id` was supplied, loads that specific `StationQueue` row directly; otherwise calls `StationQueueRepository::findRecentlyCuedSong($station, $media)` to find a matching recently-cued queue entry, and if found, backfills its `media`/`playlist` associations if they were missing (using the payload's `playlist_id` if supplied) before persisting. If a `StationQueue` row was resolved (either way), calls `queueRepo->trackPlayed($station, $sq)` and builds the `SongHistory` via `SongHistory::fromQueue($sq)` (full linkage to the original queue/request/playlist chain). If no queue row could be resolved at all, falls back to a bare `new SongHistory($station, $media)` (media linkage only, no queue/playlist/request chain), attaching `playlist_id`'s playlist directly if supplied.
  - Persists the new `SongHistory` row, calls `SongHistoryRepository::changeCurrentSong($station, $historyRow)` (flips whatever "now playing" pointer the rest of the system uses), flushes, then forces an immediate refresh of the `NowPlayingCache` for the station (`nowPlayingCache->forceUpdate($station)` — synchronous cache rebuild, not deferred).
- **Response body**: `true` on success; on any thrown exception, HTTP 400 (silently ignored by the fire-and-forget Liquidsoap caller).

### D.6 `CopyCommand` — `cp`
- **Liquidsoap caller**: `azuracast.media_protocol` (the `media:` protocol handler, C.10), payload `{"uri": arg}` — only invoked when `settings.azuracast.media_path() == "api"` (non-local storage). Timeout for this specific call is dynamically computed by the caller as milliseconds remaining until the request's `maxtime`, not a fixed value.
- **PHP behavior**: throws `InvalidArgumentException('No URI provided.')` if `uri` is empty. Otherwise resolves the station's media filesystem (`StationFilesystems::getMediaFilesystem`) and calls `getLocalPath($uri)` on it — for a remote/non-local storage adapter, this presumably materializes/downloads the file to a local temp path (the exact mechanics of `getLocalPath` live in `StationFilesystems`, outside the reviewed file set, but its *contract* here is: given a storage-relative URI, return a path on local disk that Liquidsoap's own file-reading code can open directly). Returns `{"uri": {localPath}, "isTemp": !mediaFs->isLocal()}` — `isTemp` is simply the negation of "is this filesystem adapter local" (i.e. any non-local backend implies the returned path is a throwaway/temporary materialization that should be treated as such).
- **Response body**: `{"uri": string, "isTemp": bool}`.
- **Liquidsoap consumption**: if `isTemp`, prefixes the URI with `"tmp:"` before returning it as the resolved `media:` protocol target (invoking Liquidsoap's own temp-file lifecycle/cleanup semantics for that prefix); otherwise uses the bare path.

### D.7 `SaveCacheCommand` — `savecache`
- **Liquidsoap caller**: `azuracast.autocue`'s branch #2 (on-the-fly autocue computation success path, C.10), payload `{"cache_key": ..., "data": {computed autocue result record}}`, fixed 5-second timeout, fire-and-forget (`_ = azuracast.api_call(...)`, return value discarded, called from an async `thread.run(fast=false, ...)`).
- **PHP behavior**: returns `false` (no-op) if `!$asAutoDj`. Extracts `cache_key` and `data` (`Types::arrayOrNull`); returns `false` if either is empty. Otherwise calls `AutoCueCache::setForCacheKey($cacheKey, $data)` and returns `true`.
- **Response body**: `true`/`false` (ignored by the fire-and-forget caller).
- **Read-side counterpart** (for completeness/exactness, even though it's not a separate HTTP command): the cache written here is read back via `AutoCueCache::getForCacheKey($cacheKey)` inside `Annotations::addCachedAutocueData` (D.1) on a **subsequent** `nextsong` call for the same media, and separately inside `azuracast.autocue`'s branch #1 check is really keyed off the `azuracast_autocue`/`azuracast_cache_key` annotations that `nextsong`'s response embeds — i.e. `savecache` and `nextsong`'s cache-annotation logic form a closed loop: first play computes+caches, every subsequent play of the same media reuses the cached values without recomputation, until/unless the cache is invalidated by other logic outside this file set.

### D.8 `AbstractCommand` — shared dispatch contract (recap for engine implementers)
Every concrete command's `doRun(Station $station, bool $asAutoDj, array $payload): mixed` is invoked only after: (a) the station's backend type is confirmed to be Liquidsoap, (b) a station-tagged logging processor is installed for the call's duration, (c) a debug log line records the short class name + `asAutoDj` + `payload`. The return value is JSON-encoded verbatim as the HTTP response body on success; any thrown exception (from `doRun` itself, or from anything it calls) is caught one layer up in `LiquidsoapAction`, logged with full context (`station`, `payload`, `as-autodj`), and converted to an HTTP 400 with `{message, file, line}`. There is no retry, idempotency key, or request-id correlation built into this transport at all — every one of the seven commands is designed to be safely re-callable (idempotent-ish by construction: `nextsong` re-queries the queue fresh each time, `feedback`/`djon`/`djoff` dedupe or no-op based on current DB state, `cp`/`savecache` are pure functions of their input) rather than relying on exactly-once delivery semantics, which matches Liquidsoap's own fire-and-forget / retry-on-null calling style throughout the `.liq` layer.