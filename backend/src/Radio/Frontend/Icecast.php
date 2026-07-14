<?php

declare(strict_types=1);

namespace App\Radio\Frontend;

use App\Entity\Api\LogType;
use App\Entity\Repository\StationMountRepository;
use App\Entity\Station;
use App\Entity\StationMount;
use App\Http\Router;
use App\Radio\Enums\StreamFormats;
use App\Radio\RemoteSupervisorClientFactory;
use App\Service\Acme;
use App\Utilities\Arrays;
use App\Xml\Writer;
use GuzzleHttp\Client;
use GuzzleHttp\Promise\Utils;
use GuzzleHttp\Psr7\Uri;
use NowPlaying\AdapterFactory;
use NowPlaying\Result\Result;
use Psr\EventDispatcher\EventDispatcherInterface;
use Psr\Http\Message\UriInterface;
use Supervisor\Exception\SupervisorException as SupervisorLibException;
use Supervisor\SupervisorInterface;
use Symfony\Component\Filesystem\Path;

class Icecast extends AbstractFrontend
{
    public const int LOGLEVEL_DEBUG = 4;
    public const int LOGLEVEL_INFO = 3;
    public const int LOGLEVEL_WARN = 2;
    public const int LOGLEVEL_ERROR = 1;

    public const string BASE_DIR = '/usr/local/share/icecast';
    public const string WEBROOT = self::BASE_DIR . '/web';
    public const string ADMINROOT = self::BASE_DIR . '/admin';

    /** Default supervisord `inet_http_server` TCP port for remote stations. */
    public const int DEFAULT_SUPERVISOR_PORT = 9002;

    public function __construct(
        protected RemoteSupervisorClientFactory $remoteSupervisorFactory,
        AdapterFactory $adapterFactory,
        StationMountRepository $stationMountRepo,
        SupervisorInterface $supervisor,
        EventDispatcherInterface $dispatcher,
        Router $router,
        Client $httpClient,
    ) {
        parent::__construct($adapterFactory, $stationMountRepo, $supervisor, $dispatcher, $router, $httpClient);
    }

    /**
     * @inheritDoc
     */
    protected function getSupervisor(Station $station): SupervisorInterface
    {
        $feConfig = $station->frontend_config;

        if (null === $feConfig->host) {
            return parent::getSupervisor($station);
        }

        return $this->remoteSupervisorFactory->getClient(
            $feConfig->host,
            $feConfig->supervisor_port ?? self::DEFAULT_SUPERVISOR_PORT,
            $feConfig->supervisor_username,
            $feConfig->supervisor_password
        );
    }

    public function reload(Station $station): void
    {
        if ($this->hasCommand($station)) {
            $programName = $this->getSupervisorFullName($station);

            try {
                $this->getSupervisor($station)->signalProcess($programName, 'HUP');
                $this->logger->info(
                    'Adapter "' . self::class . '" reloaded.',
                    ['station_id' => $station->id, 'station_name' => $station->name]
                );
            } catch (SupervisorLibException $e) {
                $this->handleSupervisorException($e, $programName, $station);
            }
        }
    }

    public function getNowPlaying(Station $station, bool $includeClients = true): Result
    {
        $feConfig = $station->frontend_config;
        $radioPort = $feConfig->port;

        if (null !== $feConfig->host) {
            // Remote station: poll the Icecast admin stats API on its own jail/host.
            $baseUrl = (new Uri())
                ->withScheme('http')
                ->withHost($feConfig->host)
                ->withPort($radioPort);
        } else {
            // Local/co-located station (default, unchanged behavior).
            $baseUrl = $this->environment->getLocalUri()
                ->withPort($radioPort);
        }

        $npAdapter = $this->adapterFactory->getIcecastAdapter($baseUrl);

        $npAdapter->setAdminPassword($feConfig->admin_pw);

        $mountPromises = [];
        $defaultMountId = null;

        foreach ($station->mounts as $mount) {
            if ($mount->is_default) {
                $defaultMountId = $mount->id;
            }

            $mountPromises[$mount->id] = $npAdapter->getNowPlayingAsync(
                $mount->name,
                $includeClients
            )->then(
                function (Result $result) use ($mount) {
                    if (!empty($result->clients)) {
                        foreach ($result->clients as $client) {
                            $client->mount = 'local_' . $mount->id;
                        }
                    }

                    $mount->listeners_total = $result->listeners->total;
                    $mount->listeners_unique = $result->listeners->unique ?? 0;
                    $this->em->persist($mount);

                    return $result;
                }
            );
        }

        $mountPromiseResults = Utils::settle($mountPromises)->wait();

        $this->em->flush();

        $defaultResult = Result::blank();
        $otherResults = [];
        foreach ($mountPromiseResults as $mountId => $result) {
            if ($mountId === $defaultMountId) {
                $defaultResult = $result['value'] ?? Result::blank();
            } else {
                $otherResults[] = $result['value'] ?? Result::blank();
            }
        }

        foreach ($otherResults as $otherResult) {
            $defaultResult = $defaultResult->merge($otherResult);
        }

        return $defaultResult;
    }

    public function getConfigurationPath(Station $station): string
    {
        return $station->getRadioConfigDir() . '/icecast.xml';
    }

    public function getCurrentConfiguration(Station $station): string
    {
        $config = $this->getConfigurationArray($station);
        return Writer::toString($config, 'icecast', false);
    }

    protected function processCustomConfig(?string $customConfigRaw): array|false
    {
        $customConfParsed = parent::processCustomConfig($customConfigRaw);

        if (false !== $customConfParsed) {
            // Special handling for aliases.
            if (isset($customConfParsed['paths']['alias'])) {
                $alias = (array)$customConfParsed['paths']['alias'];
                if (!is_numeric(key($alias))) {
                    $alias = [$alias];
                }
                $customConfParsed['paths']['alias'] = $alias;
            }
        }

        return $customConfParsed;
    }

    protected function getConfigurationArray(Station $station): array
    {
        $frontendConfig = $station->frontend_config;
        $configDir = $station->getRadioConfigDir();

        $settingsBaseUrl = $this->settingsRepo->readSettings()->getBaseUrlAsUri();
        $baseUrl = $settingsBaseUrl ?? new Uri('http://localhost');

        [$certPath, $certKey] = Acme::getCertificatePaths();

        $config = [
            'location' => 'AzuraCast',
            'admin' => 'icemaster@localhost',
            'hostname' => $baseUrl->getHost(),
            'limits' => [
                'clients' => !empty($frontendConfig->max_listeners) ? $frontendConfig->max_listeners * 2 : 2500,
                'max-listeners' => $frontendConfig->max_listeners ?? -1,
                'sources' => $station->mounts->count(),
                'queue-size' => 524288,
                'client-timeout' => 30,
                'header-timeout' => 15,
                'source-timeout' => 10,
                'burst-size' => 65535,
            ],
            'authentication' => [
                'source-password' => $frontendConfig->source_pw,
                'relay-password' => $frontendConfig->relay_pw,
                'admin-user' => 'admin',
                'admin-password' => $frontendConfig->admin_pw,
            ],

            'listen-socket' => [
                'port' => $frontendConfig->port,
            ],

            'mount' => [],
            'fileserve' => 1,
            'paths' => [
                'basedir' => self::BASE_DIR,
                'logdir' => $configDir,
                'webroot' => self::WEBROOT,
                'adminroot' => self::ADMINROOT,
                'pidfile' => $configDir . '/icecast.pid',
                'alias' => [
                    [
                        '@source' => '/',
                        '@dest' => '/status.xsl',
                    ],
                ],
                'ssl-private-key' => $certKey,
                'ssl-certificate' => $certPath,
                // phpcs:disable Generic.Files.LineLength
                'ssl-allowed-ciphers' => 'ECDH+AESGCM:DH+AESGCM:ECDH+AES256:DH+AES256:ECDH+AES128:DH+AES:RSA+AESGCM:RSA+AES:!aNULL:!MD5:!DSS',
                // phpcs:enable
                'deny-ip' => $this->writeIpBansFile($station),
                'deny-agents' => $this->writeUserAgentBansFile($station),
                'x-forwarded-for' => '127.0.0.1',
            ],
            'logging' => [
                'accesslog' => 'icecast_access.log',
                'errorlog' => '/dev/stderr',
                'loglevel' => $this->environment->isProduction() ? self::LOGLEVEL_WARN : self::LOGLEVEL_INFO,
                'logsize' => 10000,
            ],
            'security' => [
                'chroot' => 0,
            ],
        ];

        $bannedCountries = $frontendConfig->banned_countries ?? [];
        $allowedIps = $this->getIpsAsArray($frontendConfig->allowed_ips);

        $useListenerAuth = !empty($bannedCountries) || !empty($allowedIps);
        $charset = match ($station->backend_config->charset) {
            'ISO-8859-1' => 'ISO8859-1',
            default => 'UTF8',
        };

        /** @var StationMount $mountRow */
        foreach ($station->mounts as $mountRow) {
            $mount = [
                '@type' => 'normal',
                'mount-name' => $mountRow->name,
                'charset' => $charset,
                'stream-name' => $station->name,
                'listenurl' => $this->getUrlForMount($station, $mountRow),
            ];

            if ($station->max_bitrate !== 0) {
                $maxBitrateInBps = $station->max_bitrate * 1024 + 2500;
                $mount['limit-rate'] = $maxBitrateInBps;
            }

            if (!empty($station->description)) {
                $mount['stream-description'] = $station->description;
            }

            if (!empty($station->url)) {
                $mount['stream-url'] = $station->url;
            }

            if (!empty($station->genre)) {
                $mount['genre'] = $station->genre;
            }

            if (!$mountRow->is_visible_on_public_pages) {
                $mount['hidden'] = 1;
            }

            if (!empty($mountRow->intro_path)) {
                $introPath = $mountRow->intro_path;
                // The intro path is appended to webroot, so the path should be relative to it.
                $mount['intro'] = Path::makeRelative(
                    $station->getRadioConfigDir() . '/' . $introPath,
                    self::WEBROOT
                );
            }

            if (!empty($mountRow->fallback_mount)) {
                $mount['fallback-mount'] = $mountRow->fallback_mount;
                $mount['fallback-override'] = 1;
            } elseif ($mountRow->enable_autodj) {
                $autoDjFormat = $mountRow->autodj_format ?? StreamFormats::default();
                $autoDjBitrate = $mountRow->autodj_bitrate;

                $mount['fallback-mount'] = '/fallback-[' . $autoDjBitrate . '].' . $autoDjFormat->getExtension();
                $mount['fallback-override'] = 1;
            }

            if ($mountRow->max_listener_duration) {
                $mount['max-listener-duration'] = $mountRow->max_listener_duration;
            }

            $mountFrontendConfig = trim($mountRow->frontend_config ?? '');
            if (!empty($mountFrontendConfig)) {
                $mountConf = $this->processCustomConfig($mountFrontendConfig);
                if (false !== $mountConf) {
                    $mount = Arrays::arrayMergeRecursiveDistinct($mount, $mountConf);
                }
            }

            $mountRelayUri = $mountRow->getRelayUrlAsUri();
            if (null !== $mountRelayUri) {
                // Relay URLs may embed credentials for an authenticated upstream source via
                // standard URI userinfo syntax (e.g. icecast://source:hackme@host:port/mount) --
                // getRelayUrlAsUri() already parses this out via PSR-7's UriInterface, it just
                // wasn't being read here. Icecast's <relay> block accepts optional <username>/
                // <password> children for exactly this case; without them, relaying from a
                // password-protected upstream source silently fails (Icecast connects
                // unauthenticated and gets rejected).
                $relayUserInfo = $mountRelayUri->getUserInfo();
                [$relayUsername, $relayPassword] = ('' !== $relayUserInfo)
                    ? array_pad(explode(':', $relayUserInfo, 2), 2, null)
                    : [null, null];

                $config['relay'][] = array_filter([
                    'server' => $mountRelayUri->getHost(),
                    'port' => $mountRelayUri->getPort(),
                    'mount' => $mountRelayUri->getPath(),
                    'local-mount' => $mountRow->name,
                    'username' => $relayUsername,
                    'password' => $relayPassword,
                ]);
            }

            if ($useListenerAuth) {
                $mount['authentication'][] = [
                    '@type' => 'url',
                    'option' => [
                        [
                            '@name' => 'listener_add',
                            '@value' => $this->getAuthenticationUrl($station),
                        ],
                        [
                            '@name' => 'auth_header',
                            '@value' => 'icecast-auth-user: 1',
                        ],
                    ],
                ];
            }

            $config['mount'][] = $mount;
        }

        $customConfParsed = $this->processCustomConfig($frontendConfig->custom_config);
        if (false !== $customConfParsed) {
            $config = Arrays::arrayMergeRecursiveDistinct($config, $customConfParsed);
        }

        return $config;
    }

    protected function getAuthenticationUrl(Station $station): string
    {
        return (string)Router::resolveUri(
            $this->environment->getInternalUri(),
            $this->router->named(
                'api:internal:listener-auth',
                [
                    'station_id' => $station->id,
                    'api_auth' => $station->adapter_api_key,
                ]
            ),
            true
        );
    }

    public function getCommand(Station $station): ?string
    {
        // `getBinary()` (inherited, no-arg) can only check for the binary's presence on the
        // *local* filesystem, i.e. the PHP/webapp jail. For a remote station, Icecast runs in
        // a different jail entirely, so a local `file_exists()` check would always fail and
        // incorrectly make `hasCommand()`/`isRunning()`/`start()`/`stop()` no-ops for it. Since
        // `getBinary()` has no Station context (it's also called station-agnostically from
        // `isInstalled()`), the remote case is special-cased here instead: we trust that the
        // remote jail has Icecast installed at the standard path and always return a command,
        // without gating on any local file check.
        if (null !== $station->frontend_config->host) {
            return sprintf(
                '%s -c %s',
                escapeshellcmd('/usr/local/bin/icecast'),
                escapeshellarg($this->getConfigurationPath($station))
            );
        }

        $binary = $this->getBinary();
        if ($binary === null) {
            return null;
        }

        return sprintf(
            '%s -c %s',
            escapeshellcmd($binary),
            escapeshellarg($this->getConfigurationPath($station))
        );
    }

    /**
     * @inheritDoc
     */
    public function getBinary(): ?string
    {
        $newPath = '/usr/local/bin/icecast';
        $legacyPath = '/usr/bin/icecast2';

        if ($this->environment->isDocker() || file_exists($newPath)) {
            return $newPath;
        }

        if (file_exists($legacyPath)) {
            return $legacyPath;
        }

        return null;
    }

    public function getAdminUrl(Station $station, ?UriInterface $baseUrl = null): UriInterface
    {
        $publicUrl = $this->getPublicUrl($station, $baseUrl);
        return $publicUrl
            ->withPath($publicUrl->getPath() . '/admin.html');
    }

    public function getLogTypes(Station $station): array
    {
        $stationConfigDir = $station->getRadioConfigDir();

        return [
            new LogType(
                'icecast_access_log',
                __('Icecast Access Log'),
                $stationConfigDir . '/icecast_access.log',
                true
            ),
            new LogType(
                'icecast_error_log',
                __('Icecast Error Log'),
                $stationConfigDir . '/icecast.log',
                true
            ),
            new LogType(
                'icecast_xml',
                __('Icecast Configuration'),
                $stationConfigDir . '/icecast.xml',
                false,
            ),
        ];
    }
}
