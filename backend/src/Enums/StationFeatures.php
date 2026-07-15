<?php

declare(strict_types=1);

namespace App\Enums;

use App\Entity\Settings;
use App\Entity\Station;
use App\Exception\StationUnsupportedException;
use App\Radio\Enums\BackendAdapters;

enum StationFeatures
{
    case EngineConfig;
    case Media;
    case Sftp;
    case MountPoints;
    case RemoteRelays;
    case HlsStreams;
    case Streamers;
    case Webhooks;
    case Podcasts;
    case OnDemand;
    case Requests;

    public function supportedForStation(
        Station $station,
        Settings $settings
    ): bool {
        $backendEnabled = $station->backend_type->isEnabled();

        return match ($this) {
            self::Media => $backendEnabled,
            // The structured TOML editor is StreamEngine-only. It can't inject arbitrary code --
            // it only round-trips already-editable structured fields -- so it doesn't need to be
            // gated behind an `enable_*_editing`-style global toggle.
            self::EngineConfig => $backendEnabled && BackendAdapters::StreamEngine === $station->backend_type,
            self::Streamers => $backendEnabled && $station->enable_streamers,
            self::Sftp => $backendEnabled && $station->media_storage_location->adapter->isLocal(),
            self::MountPoints => $station->frontend_type->supportsMounts(),
            self::HlsStreams => $backendEnabled && $station->enable_hls,
            self::Requests => $backendEnabled && $station->enable_requests,
            self::OnDemand => $settings->enable_all_webhooks && $station->enable_on_demand,
            self::Webhooks, self::Podcasts, self::RemoteRelays => true,
        };
    }

    /**
     * @param Station $station
     * @param Settings $settings
     * @return void
     * @throws StationUnsupportedException
     */
    public function assertSupportedForStation(Station $station, Settings $settings): void
    {
        if (!$this->supportedForStation($station, $settings)) {
            throw match ($this) {
                self::Requests => StationUnsupportedException::requests(),
                self::OnDemand => StationUnsupportedException::onDemand(),
                default => StationUnsupportedException::generic(),
            };
        }
    }
}
