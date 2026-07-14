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
use App\Radio\Enums\AudioQueues;
use App\Radio\FallbackFile;
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
            ...$harborLines,
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
