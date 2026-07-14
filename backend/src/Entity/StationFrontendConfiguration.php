<?php

declare(strict_types=1);

namespace App\Entity;

use App\Doctrine\AbstractArrayEntity;
use App\Utilities\Strings;
use App\Utilities\Types;
use OpenApi\Attributes as OA;

#[OA\Schema(schema: "StationFrontendConfiguration", type: "object")]
final class StationFrontendConfiguration extends AbstractArrayEntity
{
    #[OA\Property]
    public ?string $custom_config = null {
        set => Types::stringOrNull($value, true);
    }

    #[OA\Property]
    public string $source_pw;

    #[OA\Property]
    public string $admin_pw;

    #[OA\Property]
    public string $relay_pw;

    #[OA\Property]
    public string $streamer_pw;

    /**
     * The remote host/address where this station's Icecast process (and its supervisord
     * instance) runs, if it is not co-located with the PHP application in the same jail/host.
     *
     * Null (the default) means "co-located with PHP" — the existing local Unix-socket
     * supervisor and local `getLocalUri()` polling behavior is used, unchanged.
     */
    #[OA\Property]
    public ?string $host = null {
        set => Types::stringOrNull($value, true);
    }

    /**
     * The TCP port of the remote supervisord's `inet_http_server`, when `$host` is set.
     *
     * Defaults to 9002 when unset; consumers should read this via `$supervisor_port ?? 9002`
     * rather than relying on a stored default, since this field is meaningless (and left null)
     * for co-located stations.
     */
    #[OA\Property]
    public ?int $supervisor_port = null {
        set (int|string|null $value) => Types::intOrNull($value);
    }

    #[OA\Property]
    public ?string $supervisor_username = null {
        set => Types::stringOrNull($value, true);
    }

    #[OA\Property]
    public ?string $supervisor_password = null {
        set => Types::stringOrNull($value, true);
    }

    public function ensurePasswordsAreSet(): void
    {
        $autoAssignPasswords = [
            'source_pw',
            'admin_pw',
            'relay_pw',
            'streamer_pw',
        ];

        foreach ($autoAssignPasswords as $autoAssignPassword) {
            if (empty($this->$autoAssignPassword)) {
                $this->$autoAssignPassword = Strings::generatePassword();
            }
        }

        // The remote supervisor password is only auto-assigned when this station is actually
        // configured to run against a remote host; local/default installs never touch it.
        if (null !== $this->host && empty($this->supervisor_password)) {
            $this->supervisor_password = Strings::generatePassword();
        }
    }

    #[OA\Property]
    public ?int $port = null {
        set (int|string|null $value) => Types::intOrNull($value);
    }

    #[OA\Property]
    public ?int $max_listeners = null {
        set (int|string|null $value) => Types::intOrNull($value);
    }

    #[OA\Property]
    public ?string $banned_ips = null {
        set => Types::stringOrNull($value, true);
    }

    #[OA\Property]
    public ?string $banned_user_agents = null {
        set => Types::stringOrNull($value, true);
    }

    #[OA\Property(
        items: new OA\Items(type: 'string'),
    )]
    public ?array $banned_countries = null;

    #[OA\Property]
    public ?string $allowed_ips = null {
        set => Types::stringOrNull($value, true);
    }

    #[OA\Property]
    public ?string $sc_license_id = null {
        set => Types::stringOrNull($value, true);
    }

    #[OA\Property]
    public ?string $sc_user_id = null {
        set => Types::stringOrNull($value, true);
    }

    /**
     * @inheritDoc
     */
    public static function merge(
        ?array $sourceData,
        array|AbstractArrayEntity|null $newData
    ): array|null {
        $arrayEntity = new self((array)$sourceData);
        if ($newData !== null) {
            $arrayEntity->fromArray($newData);
        }

        // Generate defaults if not set.
        $arrayEntity->ensurePasswordsAreSet();

        return $arrayEntity->toArray(true);
    }
}
