<?php

declare(strict_types=1);

namespace App\Media\AlbumArtHandler;

use App\Entity\Interfaces\SongInterface;
use GuzzleHttp\Client;
use GuzzleHttp\RequestOptions;
use RuntimeException;

/**
 * Album art via Deezer's public search API — keyless and free (no account
 * or configuration), with generous rate limits (~50 requests/5s per IP)
 * and up-to-1000px covers (`cover_xl`). Registered between the
 * MusicBrainz/Cover Art Archive handler and the Last.fm one (see
 * config/events.php): CAA offers canonical scans but misses obscure
 * releases; Deezer catches most of the remainder before Last.fm's
 * anything-goes catalog gets the final word.
 */
final class DeezerAlbumArtHandler extends AbstractAlbumArtHandler
{
    public function __construct(
        private readonly Client $httpClient
    ) {
    }

    protected function getServiceName(): string
    {
        return 'Deezer';
    }

    protected function getAlbumArt(SongInterface $song): ?string
    {
        // Prefer an album-scoped search when the album is known (matches
        // the exact release's art); otherwise search by artist + track.
        if (!empty($song->album)) {
            $query = sprintf('artist:"%s" album:"%s"', $song->artist ?? '', $song->album);
        } else {
            $query = sprintf('artist:"%s" track:"%s"', $song->artist ?? '', $song->title ?? '');
        }

        $response = $this->httpClient->request(
            'GET',
            'https://api.deezer.com/search',
            [
                RequestOptions::QUERY => [
                    'q' => $query,
                    'limit' => 1,
                ],
                RequestOptions::TIMEOUT => 10,
            ]
        );

        $body = json_decode((string)$response->getBody(), true, 512, JSON_THROW_ON_ERROR);

        if (isset($body['error'])) {
            // Deezer reports quota exhaustion as an error object ("Quota
            // limit exceeded"). Phrase the exception so the abstract
            // handler's rate-limit detection treats it as a soft skip
            // (this handler yields, the next one in the chain still runs)
            // instead of a hard failure.
            throw new RuntimeException(
                'rate limit / API error from Deezer: ' . json_encode($body['error'])
            );
        }

        $album = $body['data'][0]['album'] ?? null;
        if (!is_array($album)) {
            return null;
        }

        $art = $album['cover_xl'] ?? $album['cover_big'] ?? $album['cover'] ?? null;
        return is_string($art) && '' !== $art ? $art : null;
    }
}
