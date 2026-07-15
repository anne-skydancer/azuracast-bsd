<?php

declare(strict_types=1);

namespace App\Radio\Enums;

use App\Radio\Frontend\AbstractFrontend;
use App\Radio\Frontend\Icecast;
use OpenApi\Attributes as OA;

#[OA\Schema(type: 'string')]
enum FrontendAdapters: string implements AdapterTypeInterface
{
    case Icecast = 'icecast';
    case Remote = 'remote';

    public function getValue(): string
    {
        return $this->value;
    }

    public function getName(): string
    {
        return match ($this) {
            self::Icecast => 'Icecast 2.4',
            self::Remote => 'Remote',
        };
    }

    /**
     * @return class-string<AbstractFrontend>|null
     */
    public function getClass(): ?string
    {
        return match ($this) {
            self::Icecast => Icecast::class,
            default => null
        };
    }

    public function isEnabled(): bool
    {
        return self::Remote !== $this;
    }

    public function supportsMounts(): bool
    {
        return self::Icecast === $this;
    }

    public function supportsReload(): bool
    {
        return self::Icecast === $this;
    }

    public static function default(): self
    {
        return self::Icecast;
    }
}
