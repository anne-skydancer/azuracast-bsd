<?php

declare(strict_types=1);

namespace App\Sync\Task;

use App\Radio\Adapters;
use App\Radio\AutoDJ\Scheduler;

final class EnforceBroadcastTimesTask extends AbstractTask
{
    public function __construct(
        private readonly Scheduler $scheduler,
        private readonly Adapters $adapters,
    ) {
    }

    public static function getSchedulePattern(): string
    {
        return self::SCHEDULE_EVERY_MINUTE;
    }

    public function run(bool $force = false): void
    {
        foreach ($this->iterateStations() as $station) {
            if (!$station->backend_type->isEnabled()) {
                continue;
            }

            $currentStreamer = $station->current_streamer;
            if (null === $currentStreamer) {
                continue;
            }

            if (!$this->scheduler->canStreamerStreamNow($currentStreamer)) {
                $adapter = $this->adapters->getBackendAdapter($station);

                $adapter?->disconnectStreamer($station);
            }
        }
    }
}
