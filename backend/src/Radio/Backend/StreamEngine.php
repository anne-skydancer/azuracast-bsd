<?php

declare(strict_types=1);

namespace App\Radio\Backend;

use App\Entity\Api\LogType;
use App\Entity\Station;
use App\Entity\StationStreamer;
use App\Http\Router;
use App\Nginx\CustomUrls;
use App\Radio\AbstractLocalAdapter;
use App\Radio\Configuration;
use App\Radio\Enums\AudioProcessingMethods;
use App\Radio\Enums\AudioQueues;
use App\Radio\Enums\FrontendAdapters;
use App\Radio\Enums\StreamFormats;
use App\Radio\FallbackFile;
use App\Radio\StereoTool;
use GuzzleHttp\Client;
use LogicException;
use Psr\EventDispatcher\EventDispatcherInterface;
use Psr\Http\Message\UriInterface;
use Supervisor\SupervisorInterface;
use Symfony\Component\Process\Process;

/**
 * Adapter for the new Rust-based AzuraCast Engine, which replaces Liquidsoap's telnet API
 * with a small HTTP control API and a structured TOML configuration file.
 *
 * Config generation covers real AutoDJ decode/crossfade/replaygain (Phase 3) and live DJ
 * harbor input (Phase 4, `[harbor]` section below) as of this phase. Icecast/Shoutcast
 * network output and HLS are still a later-phase concern; see engine/SPEC.md.
 */
final class StreamEngine extends AbstractLocalAdapter implements BackendInterface
{
    private const string AUTH_HEADER = 'X-Engine-Api-Key';

    public function __construct(
        private readonly FallbackFile $fallbackFile,
        SupervisorInterface $supervisor,
        EventDispatcherInterface $dispatcher,
        Router $router,
        Client $httpClient,
    ) {
        parent::__construct($supervisor, $dispatcher, $router, $httpClient);
    }

    /**
     * @inheritDoc
     */
    public function getConfigurationPath(Station $station): string
    {
        return $station->getRadioConfigDir() . '/engine.toml';
    }

    /**
     * Build the TOML configuration needed for the engine to start, expose its control API, make
     * callbacks to PHP, and (as of Phase 3) apply the station's replaygain and crossfade
     * settings. Richer configuration (encoding, HLS, etc.) is a later-phase concern; see
     * engine/SPEC.md.
     *
     * @inheritDoc
     */
    public function getCurrentConfiguration(Station $station): string
    {
        $apiKey = $station->adapter_api_key ?? '';
        $callbackBaseUrl = (string)$this->environment->getInternalUri();
        $logPath = $station->getRadioConfigDir() . '/engine.log';

        $backendConfig = $station->backend_config;

        // `enable_replaygain_metadata`'s getter already forces `false` when `enable_auto_cue`
        // is true (see StationBackendConfiguration), so this already reflects the effective
        // value -- no need to re-derive the override here.
        $replaygainEnabled = $backendConfig->enable_replaygain_metadata;

        $fallbackPath = $this->fallbackFile->getFallbackPathForStation($station);

        // `getCrossfadeTypeEnum()` already encodes the `enable_auto_cue` override (forces
        // `Disabled` aka `"none"`); its enum value ("normal"/"smart"/"none") maps directly onto
        // what the Rust engine's `CrossfadeConfig::mode()` expects, since it treats both
        // "disabled" and "none" as CrossfadeMode::Disabled.
        $crossfadeMode = $backendConfig->getCrossfadeTypeEnum()->value;

        // Deliberately the raw `crossfade` field (SPEC.md's `default_fade`), NOT
        // `getCrossfadeDuration()`'s 1.5x-scaled value (SPEC.md's `default_cross`) -- the
        // engine's `fade_seconds` corresponds to `default_fade` (see engine/src/crossfade.rs).
        $fadeSeconds = $backendConfig->crossfade;

        // Phase 4: live DJ harbor input. Mirrors Liquidsoap's `input.harbor(...)` config (SPEC.md
        // B.4) -- `enabled` gates the whole harbor listener off entirely when the station has no
        // streamers (matching `writeHarborConfiguration`'s no-op-if-disabled behavior), rather
        // than starting a listener nobody can use. `buffer_secs`/`max_buffer_secs` are only
        // emitted when `dj_buffer != 0`, matching Liquidsoap's own conditional emission of
        // `buffer=`/`max=` (omitted entirely otherwise, letting the engine fall back to its own
        // default).
        $harborEnabled = $station->enable_streamers;
        $djBuffer = $backendConfig->dj_buffer;

        // Audio post-processing: `nrj` (normalize + compress, hand-rolled DSP in the engine, see
        // engine/src/audio_processing.rs) or `stereo_tool` (piped through an operator-provided,
        // separately-licensed `stereo_tool` binary as a subprocess -- the engine does not attempt
        // to reimplement Stereo Tool's own processing, only to pipe audio through it). Emitted as
        // `method = "none"` (i.e. the whole section still appears, but inert) whenever the
        // configured method isn't actually usable, rather than silently omitting the section --
        // that keeps `[audio_processing]` always present for the engine to parse unconditionally.
        $audioProcessingMethod = $backendConfig->getAudioProcessingMethodEnum();
        $audioProcessingLines = [
            '',
            '[audio_processing]',
        ];

        if (
            AudioProcessingMethods::StereoTool === $audioProcessingMethod
            && StereoTool::isReady($station)
        ) {
            $stereoToolBinary = StereoTool::getLibraryPath() . '/stereo_tool';

            // Only the standalone CLI binary is supported as a subprocess pipe target -- the
            // shared-library (.so) variant Liquidsoap could `dlopen()` and call directly has no
            // equivalent in the engine (that would require FFI bindings against Stereo Tool's own
            // undocumented, proprietary ABI). If only the .so variant is installed, post-processing
            // is simply unavailable, same as if no method were configured at all.
            if (is_file($stereoToolBinary)) {
                $audioProcessingLines[] = 'method = "stereo_tool"';
                $audioProcessingLines[] = 'include_live = ' . self::tomlBool($backendConfig->post_processing_include_live);
                $audioProcessingLines[] = 'stereo_tool_binary = ' . self::tomlString($stereoToolBinary);
                $audioProcessingLines[] = 'stereo_tool_preset_path = ' . self::tomlString(
                    $station->getRadioConfigDir() . '/' . $backendConfig->stereo_tool_configuration_path
                );

                $stereoToolLicenseKey = $backendConfig->stereo_tool_license_key ?? '';
                if ('' !== $stereoToolLicenseKey) {
                    $audioProcessingLines[] = 'stereo_tool_license_key = ' . self::tomlString($stereoToolLicenseKey);
                }
            } else {
                $audioProcessingLines[] = 'method = "none"';
            }
        } elseif (AudioProcessingMethods::Nrj === $audioProcessingMethod) {
            $audioProcessingLines[] = 'method = "nrj"';
            $audioProcessingLines[] = 'include_live = ' . self::tomlBool($backendConfig->post_processing_include_live);
        } else {
            $audioProcessingLines[] = 'method = "none"';
        }

        $harborLines = [
            '',
            '[harbor]',
            'enabled = ' . self::tomlBool($harborEnabled),
            'bind_address = "0.0.0.0"',
            'port = ' . $this->getStreamPort($station),
            'mount_point = ' . self::tomlString($backendConfig->dj_mount_point),
            'charset = ' . self::tomlString($backendConfig->charset),
        ];

        if (0 !== $djBuffer) {
            $harborLines[] = 'buffer_secs = ' . self::tomlFloat((float)$djBuffer);
            $harborLines[] = 'max_buffer_secs = ' . self::tomlFloat(max($djBuffer + 5, 10));
        }

        // Phase 5: Icecast/Shoutcast output. `[icecast_output]` is the ONE connection target
        // every local `[[mounts]]` entry pushes to -- per SPEC.md B.7, mounts don't carry their
        // own host/port/credentials, they're all paths on the station's own Icecast frontend
        // (`frontend_config->host ?? '127.0.0.1'` + `frontend_config->port`, authenticated with
        // `frontend_config->source_pw`). Omitted entirely if the station has no local frontend
        // at all (`frontend_type === Remote`, SPEC.md A.6) -- there's nothing local to push to.
        // `[[remotes]]` is a distinct concept: each StationRemote carries its OWN independent
        // host/port/mount/credentials (SPEC.md A.8), since it's relaying to a genuinely separate
        // third-party server, not the station's own frontend.
        $outputLines = [];

        $frontendConfig = $station->frontend_config;
        $isLocalFrontend = FrontendAdapters::Remote !== $station->frontend_type;

        if ($isLocalFrontend) {
            $outputLines[] = '';
            $outputLines[] = '[icecast_output]';
            $outputLines[] = 'host = ' . self::tomlString($frontendConfig->host ?? '127.0.0.1');
            $outputLines[] = 'port = ' . ($frontendConfig->port ?? 8000);
            $outputLines[] = 'source_password = ' . self::tomlString($frontendConfig->source_pw);

            foreach ($station->mounts as $mount) {
                $outputLines[] = '';
                $outputLines[] = '[[mounts]]';
                $outputLines[] = 'path = ' . self::tomlString($mount->name);
                $outputLines[] = 'format = ' . self::tomlString(($mount->autodj_format ?? StreamFormats::default())->value);
                $outputLines[] = 'bitrate = ' . ($mount->autodj_bitrate ?? 128);
                $outputLines[] = 'is_public = ' . self::tomlBool($mount->is_public);
            }
        }

        // HLS output (SPEC.md B.8). File-based, not a network protocol: the engine segments
        // directly to `station->getRadioHlsDir()`, and nginx serves that directory as-is
        // (`Nginx\ConfigWriter::writeHlsSection()`, unchanged by this). No-op (empty
        // `$hlsLines`) if `!enable_hls` or there are no HLS streams configured, mirroring B.8's
        // own early-return conditions. `share_encoders` is not implemented here, consistent with
        // every other output section in this file -- each HLS rendition gets its own independent
        // ffmpeg process, same simplification already made for `[[mounts]]`/`[[remotes]]`.
        $hlsLines = [];

        if ($station->enable_hls && $station->hls_streams->count() > 0) {
            $hlsLines[] = '';
            $hlsLines[] = '[hls]';
            $hlsLines[] = 'enabled = true';
            $hlsLines[] = 'base_dir = ' . self::tomlString($station->getRadioHlsDir());
            $hlsLines[] = 'segment_secs = ' . self::tomlFloat((float)$backendConfig->hls_segment_length);
            $hlsLines[] = 'segments_in_playlist = ' . $backendConfig->hls_segments_in_playlist;
            $hlsLines[] = 'segments_overhead = ' . $backendConfig->hls_segments_overhead;

            foreach ($station->hls_streams as $hlsStream) {
                $hlsLines[] = '';
                $hlsLines[] = '[[hls_streams]]';
                $hlsLines[] = 'name = ' . self::tomlString($hlsStream->name);
                $hlsLines[] = 'bitrate = ' . ($hlsStream->bitrate ?? 128);
            }
        }

        foreach ($station->remotes as $remote) {
            $remoteUri = $remote->getUrlAsUri();
            $outputLines[] = '';
            $outputLines[] = '[[remotes]]';
            $outputLines[] = 'host = ' . self::tomlString($remoteUri->getHost());
            $outputLines[] = 'port = ' . ($remote->source_port ?? $remoteUri->getPort() ?? 8000);
            $outputLines[] = 'mount = ' . self::tomlString($remote->source_mount ?? '/');
            if (!empty($remote->source_username)) {
                $outputLines[] = 'username = ' . self::tomlString($remote->source_username);
            }
            $outputLines[] = 'password = ' . self::tomlString($remote->source_password ?? '');
            $outputLines[] = 'format = ' . self::tomlString(($remote->autodj_format ?? StreamFormats::default())->value);
            $outputLines[] = 'bitrate = ' . ($remote->autodj_bitrate ?? 128);
            $outputLines[] = 'is_public = ' . self::tomlBool($remote->is_public);
            // Only "icecast" (standard Icecast2 source-client protocol) is implemented by the
            // engine as of this phase; legacy Shoutcast/RSAS relay targets are deferred (same
            // scope cut as Phase 4's harbor input, which also only implements Icecast2-style
            // framing) -- the engine logs and skips any `[[remotes]]` entry it doesn't recognize
            // rather than failing the whole config.
            $outputLines[] = 'protocol = "icecast"';
        }

        $lines = [
            '[station]',
            'id = ' . $station->id,
            'name = ' . self::tomlString($station->name),
            'replaygain_enabled = ' . self::tomlBool($replaygainEnabled),
            '',
            '[control_api]',
            'bind_address = "127.0.0.1"',
            'port = ' . $this->getHttpApiPort($station),
            'api_key = ' . self::tomlString($apiKey),
            '',
            '[callbacks]',
            'base_url = ' . self::tomlString($callbackBaseUrl),
            'api_key = ' . self::tomlString($apiKey),
            'station_id = ' . $station->id,
            '',
            '[paths]',
            'log_file = ' . self::tomlString($logPath),
            'fallback_file_path = ' . self::tomlString($fallbackPath),
            '',
            '[crossfade]',
            'mode = ' . self::tomlString($crossfadeMode),
            'fade_seconds = ' . self::tomlFloat($fadeSeconds),
            'high = ' . self::tomlFloat($backendConfig->crossfade_smart_high),
            'medium = ' . self::tomlFloat($backendConfig->crossfade_smart_medium),
            'margin = ' . self::tomlFloat($backendConfig->crossfade_smart_margin),
            ...$audioProcessingLines,
            ...$harborLines,
            ...$outputLines,
            ...$hlsLines,
        ];

        return implode("\n", $lines) . "\n";
    }

    private static function tomlString(string $value): string
    {
        $escaped = str_replace(['\\', '"'], ['\\\\', '\\"'], $value);
        return '"' . $escaped . '"';
    }

    private static function tomlBool(bool $value): string
    {
        return $value ? 'true' : 'false';
    }

    /**
     * Format a number as a TOML float literal (always includes a decimal point, so e.g. `2.0`
     * is never emitted as the TOML integer `2` -- the engine's config fields are typed `f64`).
     */
    private static function tomlFloat(float $value): string
    {
        return number_format($value, 2, '.', '');
    }

    /**
     * Returns the internal port used to relay requests and other changes from AzuraCast to the
     * engine's control API. Identical fallback logic to Liquidsoap::getHttpApiPort().
     *
     * @param Station $station
     *
     * @return int The port number to use for this station.
     */
    public function getHttpApiPort(Station $station): int
    {
        $settings = $station->backend_config;
        return $settings->telnet_port ?? ($this->getStreamPort($station) - 1);
    }

    /**
     * Returns the port used for DJs/Streamers to connect to the engine for broadcasting.
     *
     * @param Station $station
     *
     * @return int The port number to use for this station.
     */
    public function getStreamPort(Station $station): int
    {
        $djPort = $station->backend_config->dj_port;
        if (null !== $djPort) {
            return $djPort;
        }

        // Default to frontend port + 5
        $frontendConfig = $station->frontend_config;
        $frontendPort = $frontendConfig->port ?? (8000 + (($station->id - 1) * 10));

        return $frontendPort + 5;
    }

    /**
     * StreamEngine has no Liquidsoap-style command DSL to send raw strings to. All control-API
     * operations are exposed as typed methods (skip/enqueue/isQueueEmpty/updateMetadata/
     * disconnectStreamer) that each call their own dedicated HTTP endpoint directly. This method
     * exists only to satisfy BackendInterface's signature for callers written against Liquidsoap
     * (e.g. the admin telnet debug tool).
     *
     * @param Station $station
     * @param string $commandStr
     *
     * @return string[]
     *
     * @throws LogicException Always.
     */
    public function command(Station $station, string $commandStr): array
    {
        throw new LogicException(
            'StreamEngine does not support raw command strings; use the typed methods instead.'
        );
    }

    /**
     * @inheritdoc
     */
    public function getCommand(Station $station): string
    {
        $binary = $this->getBinary();

        return sprintf(
            '%s --config %s',
            escapeshellcmd($binary),
            escapeshellarg($this->getConfigurationPath($station))
        );
    }

    /**
     * @inheritdoc
     */
    public function getEnvironmentVariables(Station $station): array
    {
        return [];
    }

    /**
     * @inheritDoc
     */
    public function getBinary(): string
    {
        return '/usr/local/bin/azuracast-engine';
    }

    public function getVersion(): ?string
    {
        $binary = $this->getBinary();

        $process = new Process([$binary, '--version']);
        $process->run();

        if (!$process->isSuccessful()) {
            return null;
        }

        return preg_match('/^AzuraCast Engine (.+)$/im', $process->getOutput(), $matches)
            ? $matches[1]
            : null;
    }

    public function getHlsUrl(Station $station, ?UriInterface $baseUrl = null): UriInterface
    {
        $baseUrl ??= $this->router->getBaseUrl();
        return $baseUrl->withPath(
            $baseUrl->getPath() . CustomUrls::getHlsUrl($station) . '/live.m3u8'
        );
    }

    public function isQueueEmpty(
        Station $station,
        AudioQueues $queue
    ): bool {
        $result = $this->sendControlApiRequest(
            $station,
            'GET',
            sprintf('/queue/%s/empty', $queue->value)
        );

        return (bool)($result['empty'] ?? true);
    }

    /**
     * @return string[]
     */
    public function enqueue(
        Station $station,
        AudioQueues $queue,
        string $musicFile
    ): array {
        $result = $this->sendControlApiRequest(
            $station,
            'POST',
            sprintf('/queue/%s/push', $queue->value),
            ['uri' => $musicFile]
        );

        return self::encodeResult($result);
    }

    /**
     * @return string[]
     */
    public function skip(Station $station): array
    {
        $result = $this->sendControlApiRequest($station, 'POST', '/skip');

        return self::encodeResult($result);
    }

    /**
     * @return string[]
     */
    public function updateMetadata(Station $station, array $newMeta): array
    {
        $result = $this->sendControlApiRequest($station, 'POST', '/metadata', $newMeta);

        return self::encodeResult($result);
    }

    /**
     * Tell the engine to disconnect the current live streamer.
     *
     * @param Station $station
     *
     * @return string[]
     */
    public function disconnectStreamer(Station $station): array
    {
        $currentStreamer = $station->current_streamer;
        $disconnectTimeout = $station->disconnect_deactivate_streamer;

        if ($currentStreamer instanceof StationStreamer && $disconnectTimeout > 0) {
            $currentStreamer->deactivateFor($disconnectTimeout);

            $this->em->persist($currentStreamer);
            $this->em->flush();
        }

        $result = $this->sendControlApiRequest($station, 'POST', '/streamer/disconnect');

        return self::encodeResult($result);
    }

    public function getWebStreamingUrl(Station $station, UriInterface $baseUrl): UriInterface
    {
        $djMount = $station->backend_config->dj_mount_point;

        return $baseUrl
            ->withScheme('wss')
            ->withPath($baseUrl->getPath() . CustomUrls::getWebDjUrl($station) . $djMount);
    }

    public function verifyConfig(string $config): void
    {
        $binary = $this->getBinary();

        $process = new Process([
            $binary,
            '--check-config',
            '-',
        ]);

        $process->setInput($config);
        $process->run();

        if (0 !== $process->getExitCode()) {
            throw new LogicException($process->getOutput());
        }
    }

    public function getSupervisorProgramName(Station $station): string
    {
        return Configuration::getSupervisorProgramName($station, 'backend');
    }

    public function getLogTypes(Station $station): array
    {
        $stationConfigDir = $station->getRadioConfigDir();

        return [
            new LogType(
                'engine_log',
                __('Engine Log'),
                $stationConfigDir . '/engine.log',
                true
            ),
            new LogType(
                'engine_toml',
                __('Engine Configuration'),
                $stationConfigDir . '/engine.toml',
                false
            ),
        ];
    }

    /**
     * Execute a request against the engine's HTTP control API for the given station.
     *
     * @param Station $station
     * @param string $method
     * @param string $path
     * @param array|null $jsonBody
     *
     * @return array Decoded JSON response body, or an empty array if the body wasn't a JSON object.
     */
    private function sendControlApiRequest(
        Station $station,
        string $method,
        string $path,
        ?array $jsonBody = null
    ): array {
        $apiUri = $this->environment->getLocalUri()
            ->withPort($this->getHttpApiPort($station))
            ->withPath($path);

        $options = [
            'headers' => [
                self::AUTH_HEADER => $station->adapter_api_key,
            ],
        ];

        if (null !== $jsonBody) {
            $options['json'] = $jsonBody;
        }

        $response = $this->httpClient->request($method, $apiUri, $options);

        $decoded = json_decode($response->getBody()->getContents(), true);
        return is_array($decoded) ? $decoded : [];
    }

    /**
     * Shape a decoded JSON control-API response into the string[] return contract shared with
     * Liquidsoap's command()-derived methods, so existing call sites (which mostly log or
     * concatenate the result) keep working unmodified.
     *
     * @param array $result
     *
     * @return string[]
     */
    private static function encodeResult(array $result): array
    {
        $encoded = json_encode($result);
        return [is_string($encoded) ? $encoded : '{}'];
    }
}
