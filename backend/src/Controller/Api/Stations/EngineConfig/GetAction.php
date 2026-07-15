<?php

declare(strict_types=1);

namespace App\Controller\Api\Stations\EngineConfig;

use App\Controller\SingleActionInterface;
use App\Http\Response;
use App\Http\ServerRequest;
use App\OpenApi;
use App\Radio\Backend\StreamEngine;
use OpenApi\Attributes as OA;
use Psr\Http\Message\ResponseInterface;

#[
    OA\Get(
        path: '/station/{station_id}/engine-config',
        operationId: 'getStationEngineConfig',
        summary: 'Get the current generated AzuraCast Engine configuration (TOML) for the station.',
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
final readonly class GetAction implements SingleActionInterface
{
    public function __construct(
        private StreamEngine $streamEngine,
    ) {
    }

    public function __invoke(
        ServerRequest $request,
        Response $response,
        array $params
    ): ResponseInterface {
        $station = $request->getStation();

        return $response->withJson([
            'config' => $this->streamEngine->getCurrentConfiguration($station),
        ]);
    }
}
