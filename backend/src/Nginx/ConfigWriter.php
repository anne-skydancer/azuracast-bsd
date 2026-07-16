<?php

declare(strict_types=1);

namespace App\Nginx;

use App\Entity\Station;
use App\Event\Nginx\WriteNginxConfiguration;
use App\Radio\Enums\FrontendAdapters;
use App\Radio\Frontend\Blocklist\BlocklistParser;
use Symfony\Component\EventDispatcher\EventSubscriberInterface;

final class ConfigWriter implements EventSubscriberInterface
{
    public function __construct(
        private readonly BlocklistParser $blocklistParser
    ) {
    }

    /**
     * @return mixed[]
     */
    public static function getSubscribedEvents(): array
    {
        return [
            WriteNginxConfiguration::class => [
                ['writeRadioSection', 35],
                ['writeWebDjSection', 30],
                ['writeHlsSection', 25],
            ],
        ];
    }

    public function writeRadioSection(WriteNginxConfiguration $event): void
    {
        $station = $event->getStation();

        // Only forward local radio
        if (FrontendAdapters::Remote === $station->frontend_type) {
            return;
        }

        $listenBaseUrl = CustomUrls::getListenUrl($station);
        $listenBaseUrlForRegex = preg_quote($listenBaseUrl);
        $port = $station->frontend_config->port;

        // In split-deployment topologies (e.g. the FreeBSD per-station
        // Icecast jails) the frontend runs on a separate host, given by
        // frontend_config->host; loopback only when genuinely co-located.
        $frontendHost = $station->frontend_config->host ?? '127.0.0.1';
        if (str_contains($frontendHost, ':')) {
            // Bracket IPv6 literals for URL syntax.
            $frontendHost = '[' . $frontendHost . ']';
        }

        $event->appendBlock(
            <<<NGINX
            location ~ ^({$listenBaseUrlForRegex}|/radio/{$port})\$ {
                return 302 \$uri/;
            }

            location ~ ^({$listenBaseUrlForRegex}|/radio/{$port})/(.*)\$ {
                include proxy_params;

                proxy_intercept_errors    on;
                proxy_next_upstream       error timeout invalid_header;
                proxy_redirect            off;
                proxy_set_header          Cookie "";
                proxy_connect_timeout     60;

                # A live audio stream must be relayed, not buffered: with
                # nginx's default buffering, listeners on the proxied
                # (notably HTTPS) path waited up to a minute for playback
                # that started instantly when reaching Icecast directly
                # (confirmed on a real install).
                proxy_buffering           off;

                proxy_set_header Host \$host/{$listenBaseUrl};

                set \$args \$args&_ic2=1;
                proxy_pass http://{$frontendHost}:{$port}/\$2?\$args;
            }
            NGINX
        );
    }

    public function writeWebDjSection(WriteNginxConfiguration $event): void
    {
        $station = $event->getStation();

        // Only forward if the station has an enabled backend (WebDJ/harbor input applies to any backend).
        if (!$station->backend_type->isEnabled()) {
            return;
        }

        $webDjBaseUrl = preg_quote(CustomUrls::getWebDjUrl($station));
        $autoDjPort = $station->backend_config->dj_port;

        $event->appendBlock(
            <<<NGINX
            # Reverse proxy the WebDJ connection.
            location ~ ^({$webDjBaseUrl}|/radio/{$autoDjPort})(/?)(.*)\$ {
                include proxy_params;

                proxy_pass http://127.0.0.1:{$autoDjPort}/$3;
            }
            NGINX
        );
    }

    public function writeHlsSection(WriteNginxConfiguration $event): void
    {
        $station = $event->getStation();

        if (!$station->enable_hls) {
            return;
        }

        $hlsBaseUrl = CustomUrls::getHlsUrl($station);
        $hlsFolder = $station->getRadioHlsDir();

        $hlsLogPath = self::getHlsLogFile($station);

        if ($this->blocklistParser->isEnabledForHls($station)) {
            $stationId = $station->id;
            $apiKey = $station->adapter_api_key;

            // Explicitly only authenticating the playlist since authenticating every segment
            // could be quite taxing and segment names are quite hard to guess anyways
            $event->appendBlock(
                <<<NGINX
                # Reverse proxy the frontend broadcast.
                location {$hlsBaseUrl} {
                    location ~ \.m3u8$ {
                        auth_request {$hlsBaseUrl}/auth;
                        access_log {$hlsLogPath} hls_json;
                    }

                    add_header 'Access-Control-Allow-Origin' '*';
                    add_header 'Cache-Control' 'no-cache';

                    alias {$hlsFolder};
                    try_files \$uri =404;
                }

                location = {$hlsBaseUrl}/auth {
                    internal;
                    proxy_pass http://127.0.0.1:6010/api/internal/{$stationId}/hls-listener-auth/{$apiKey};
                    proxy_pass_request_body off;
                    proxy_set_header Content-Length "";
                    proxy_set_header X-Real-IP \$remote_addr;
                    proxy_set_header X-Forwarded-For \$proxy_add_x_forwarded_for;
                }
                NGINX
            );

            return;
        }

        $event->appendBlock(
            <<<NGINX
            # Reverse proxy the frontend broadcast.
            location {$hlsBaseUrl} {
                location ~ \.m3u8$ {
                    access_log {$hlsLogPath} hls_json;
                }

                add_header 'Access-Control-Allow-Origin' '*';
                add_header 'Cache-Control' 'no-cache';

                alias {$hlsFolder};
                try_files \$uri =404;
            }
            NGINX
        );
    }

    public static function getHlsLogFile(Station $station): string
    {
        return $station->getRadioConfigDir() . '/hls.log';
    }
}
