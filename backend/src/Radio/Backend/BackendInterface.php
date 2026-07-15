<?php

declare(strict_types=1);

namespace App\Radio\Backend;

use App\Entity\Api\LogType;
use App\Entity\Station;
use App\Radio\Enums\AudioQueues;
use Psr\Http\Message\UriInterface;

/**
 * Common contract for the radio streaming/AutoDJ backend adapters (currently just StreamEngine).
 *
 * Concrete implementations extend App\Radio\AbstractLocalAdapter, which supplies the generic
 * process lifecycle (start/stop/restart/isRunning/getLogPath) by calling through to the
 * methods defined on this interface.
 */
interface BackendInterface
{
    /**
     * @inheritDoc
     */
    public function getConfigurationPath(Station $station): string;

    /**
     * @inheritDoc
     */
    public function getCurrentConfiguration(Station $station): string;

    /**
     * @inheritdoc
     */
    public function getCommand(Station $station): string;

    /**
     * @inheritdoc
     *
     * @return array<string, string>
     */
    public function getEnvironmentVariables(Station $station): array;

    /**
     * @inheritDoc
     */
    public function getBinary(): string;

    public function getVersion(): ?string;

    public function verifyConfig(string $config): void;

    /**
     * Returns the internal port used to relay requests and other changes from AzuraCast to the backend.
     *
     * @param Station $station
     *
     * @return int The port number to use for this station.
     */
    public function getHttpApiPort(Station $station): int;

    /**
     * Returns the port used for DJs/Streamers to connect to the backend for broadcasting.
     *
     * @param Station $station
     *
     * @return int The port number to use for this station.
     */
    public function getStreamPort(Station $station): int;

    public function getHlsUrl(Station $station, ?UriInterface $baseUrl = null): UriInterface;

    public function getWebStreamingUrl(Station $station, UriInterface $baseUrl): UriInterface;

    /**
     * Execute the specified remote command on the backend's control API.
     *
     * @param Station $station
     * @param string $commandStr
     *
     * @return string[]
     */
    public function command(Station $station, string $commandStr): array;

    public function isQueueEmpty(
        Station $station,
        AudioQueues $queue
    ): bool;

    /**
     * @return string[]
     */
    public function enqueue(
        Station $station,
        AudioQueues $queue,
        string $musicFile
    ): array;

    /**
     * @return string[]
     */
    public function skip(Station $station): array;

    /**
     * @return string[]
     */
    public function updateMetadata(Station $station, array $newMeta): array;

    /**
     * Tell the backend to disconnect the current live streamer.
     *
     * @param Station $station
     *
     * @return string[]
     */
    public function disconnectStreamer(Station $station): array;

    public function getSupervisorProgramName(Station $station): string;

    /**
     * @return LogType[]
     */
    public function getLogTypes(Station $station): array;
}
