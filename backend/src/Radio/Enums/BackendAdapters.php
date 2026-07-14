<?php

declare(strict_types=1);

namespace App\Radio\Enums;

use App\Radio\Backend\BackendInterface;
use App\Radio\Backend\Liquidsoap;
use App\Radio\Backend\StreamEngine;
use OpenApi\Attributes as OA;

#[OA\Schema(type: 'string')]
enum BackendAdapters: string implements AdapterTypeInterface
{
    case Liquidsoap = 'liquidsoap';
    case StreamEngine = 'stream_engine';
    case None = 'none';

    public function getValue(): string
    {
        return $this->value;
    }

    public function getName(): string
    {
        return match ($this) {
            self::Liquidsoap => 'Liquidsoap',
            self::StreamEngine => 'AzuraCast Engine',
            self::None => 'Disabled',
        };
    }

    /**
     * @return class-string<BackendInterface>|null
     */
    public function getClass(): ?string
    {
        return match ($this) {
            self::Liquidsoap => Liquidsoap::class,
            self::StreamEngine => StreamEngine::class,
            default => null,
        };
    }

    public function isEnabled(): bool
    {
        return self::None !== $this;
    }

    public static function default(): self
    {
        return self::Liquidsoap;
    }
}
