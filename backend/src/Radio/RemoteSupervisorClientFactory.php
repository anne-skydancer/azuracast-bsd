<?php

declare(strict_types=1);

namespace App\Radio;

use fXmlRpc\Client;
use fXmlRpc\Transport\PsrTransport;
use GuzzleHttp\Client as GuzzleClient;
use GuzzleHttp\Psr7\HttpFactory;
use Supervisor\Supervisor;
use Supervisor\SupervisorInterface;

/**
 * Builds a Supervisor XML-RPC client pointed at a remote supervisord instance exposing its
 * `[inet_http_server]` over TCP, for stations whose frontend (e.g. Icecast) runs in a
 * different jail/host than the PHP application.
 *
 * This mirrors the local, same-host Unix-socket wiring in `backend/config/services.php`
 * (`Supervisor\SupervisorInterface::class`) exactly -- same fXmlRpc\Client +
 * fXmlRpc\Transport\PsrTransport + Guzzle construction -- swapping only the underlying
 * transport (TCP host:port + optional HTTP basic auth instead of a Unix socket path).
 */
final class RemoteSupervisorClientFactory
{
    public function getClient(
        string $host,
        int $port,
        ?string $username = null,
        ?string $password = null
    ): SupervisorInterface {
        $guzzleOptions = [];

        if (null !== $username && null !== $password) {
            $guzzleOptions['auth'] = [$username, $password];
        }

        $client = new Client(
            sprintf('http://%s:%d/RPC2', self::formatHostForUri($host), $port),
            new PsrTransport(
                new HttpFactory(),
                new GuzzleClient($guzzleOptions)
            )
        );

        return new Supervisor($client);
    }

    /**
     * Formats a bare host for embedding in a `host:port` URI authority.
     *
     * IPv6 literals (e.g. `2001:8a0:6a32:2100::100`) contain colons that are
     * ambiguous with the `:port` separator, so they must be wrapped in
     * brackets (`[2001:8a0:6a32:2100::100]`) before being combined with a
     * port -- IPv4 addresses and hostnames are used as-is.
     */
    private static function formatHostForUri(string $host): string
    {
        if (str_starts_with($host, '[')) {
            // Already bracketed by the caller; don't double-bracket.
            return $host;
        }

        if (false !== filter_var($host, FILTER_VALIDATE_IP, FILTER_FLAG_IPV6)) {
            return '[' . $host . ']';
        }

        return $host;
    }
}
