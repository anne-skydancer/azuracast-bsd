<?php

declare(strict_types=1);

namespace App\Controller\Api\Stations\EngineConfig;

use App\Container\EntityManagerAwareTrait;
use App\Controller\SingleActionInterface;
use App\Entity\Api\Error;
use App\Entity\Api\Status;
use App\Http\Response;
use App\Http\ServerRequest;
use App\OpenApi;
use App\Radio\Backend\StreamEngine;
use App\Radio\Enums\CrossfadeModes;
use Devium\Toml\Toml;
use OpenApi\Attributes as OA;
use Psr\Http\Message\ResponseInterface;
use Throwable;

#[
    OA\Put(
        path: '/station/{station_id}/engine-config',
        operationId: 'putStationEngineConfig',
        summary: 'Validate and save the AzuraCast Engine configuration (TOML) for the station.',
        tags: [OpenApi::TAG_STATIONS_BROADCASTING],
        parameters: [
            new OA\Parameter(ref: OpenApi::REF_STATION_ID_REQUIRED),
        ],
        responses: [
            // TODO: API Response Body
            new OpenApi\Response\Success(),
            new OpenApi\Response\AccessDenied(),
            new OpenApi\Response\NotFound(),
            new OpenApi\Response\GenericError(),
        ]
    )
]
final class PutAction implements SingleActionInterface
{
    use EntityManagerAwareTrait;

    public function __construct(
        private readonly StreamEngine $streamEngine,
    ) {
    }

    public function __invoke(
        ServerRequest $request,
        Response $response,
        array $params
    ): ResponseInterface {
        $body = (array)$request->getParsedBody();
        $configText = (string)($body['config'] ?? '');

        $station = $this->em->refetch($request->getStation());

        // Validate the raw TOML the admin submitted the same way the old Liquidsoap PutAction
        // validated raw `.liq` script -- shell out to the engine binary's `--check-config`. This
        // catches both TOML syntax errors and semantic errors (bad enum values, out-of-range
        // numbers, etc.) using the same authoritative parser the engine itself uses.
        try {
            $this->streamEngine->verifyConfig($configText);
        } catch (Throwable $e) {
            return $response->withStatus(500)->withJson(Error::fromException($e));
        }

        // Re-parse the now-validated text on the PHP side so we can pull whitelisted values back
        // out of it. This should never fail given verifyConfig() above already accepted the text,
        // but guard it anyway since this is user-typed input.
        try {
            $parsed = Toml::decode($configText, asArray: true);
        } catch (Throwable $e) {
            return $response->withStatus(500)->withJson(Error::fromException($e));
        }

        if (!is_array($parsed)) {
            $parsed = [];
        }

        $backendConfig = $station->backend_config;

        // [station].replaygain_enabled -> enable_replaygain_metadata
        //
        // NOTE on the `enable_auto_cue` interaction (also applies to crossfade mode below):
        // `enable_replaygain_metadata`'s getter already forces `false` whenever `enable_auto_cue`
        // is true, and the TOML shown to the admin (via GetAction) always reflects that forced
        // effective value -- so a normal view-then-save round-trip is a no-op here regardless of
        // what's stored underneath. If an admin manually edits the text to re-enable replaygain
        // while auto-cue is still on, we still write the raw value they asked for; it simply
        // stays inert (masked by the forced-false getter) until auto-cue is turned off elsewhere,
        // at which point it takes effect. We deliberately do NOT reject/error in that case --
        // letting the value "naturally re-collapse" is simpler and less surprising than raising a
        // validation error over a field whose displayed value the admin never actually changed.
        $stationSection = $parsed['station'] ?? null;
        if (is_array($stationSection) && array_key_exists('replaygain_enabled', $stationSection)) {
            $backendConfig->enable_replaygain_metadata = (bool)$stationSection['replaygain_enabled'];
        }

        $crossfadeSection = $parsed['crossfade'] ?? null;
        if (is_array($crossfadeSection)) {
            // [crossfade].mode -> crossfade_type ("none" -> CrossfadeModes::Disabled, etc. --
            // the TOML string values match the enum's backing values directly).
            if (array_key_exists('mode', $crossfadeSection)) {
                $modeEnum = CrossfadeModes::tryFrom((string)$crossfadeSection['mode']);
                if (null !== $modeEnum) {
                    $backendConfig->crossfade_type = $modeEnum->value;
                }
            }

            if (array_key_exists('fade_seconds', $crossfadeSection)) {
                $backendConfig->crossfade = (float)$crossfadeSection['fade_seconds'];
            }

            if (array_key_exists('high', $crossfadeSection)) {
                $backendConfig->crossfade_smart_high = (float)$crossfadeSection['high'];
            }

            if (array_key_exists('medium', $crossfadeSection)) {
                $backendConfig->crossfade_smart_medium = (float)$crossfadeSection['medium'];
            }

            if (array_key_exists('margin', $crossfadeSection)) {
                $backendConfig->crossfade_smart_margin = (float)$crossfadeSection['margin'];
            }
        }

        $harborSection = $parsed['harbor'] ?? null;
        if (is_array($harborSection)) {
            if (array_key_exists('mount_point', $harborSection)) {
                $backendConfig->dj_mount_point = (string)$harborSection['mount_point'];
            }

            if (array_key_exists('charset', $harborSection)) {
                $backendConfig->charset = (string)$harborSection['charset'];
            }
        }

        // [harbor].buffer_secs -> dj_buffer
        //
        // getCurrentConfiguration() only emits `buffer_secs` when `dj_buffer != 0` (mirroring
        // Liquidsoap's own conditional emission of `buffer=`). So its absence here is meaningful
        // -- whether because the whole [harbor] section was removed or just the one key -- and
        // means "disable buffering", not "leave unchanged".
        if (is_array($harborSection) && array_key_exists('buffer_secs', $harborSection)) {
            $backendConfig->dj_buffer = (int)round((float)$harborSection['buffer_secs']);
        } else {
            $backendConfig->dj_buffer = 0;
        }

        $station->backend_config = $backendConfig;

        $this->em->persist($station);
        $this->em->flush();

        return $response->withJson(Status::updated());
    }
}
