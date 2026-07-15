<?php

declare(strict_types=1);

namespace App\Radio\Enums;

use App\Radio\AbstractLocalAdapter;
use App\Radio\Backend\BackendInterface;
use App\Radio\Backend\StreamEngine;
use OpenApi\Attributes as OA;

#[OA\Schema(type: 'string')]
enum BackendAdapters: string implements AdapterTypeInterface
{
    case StreamEngine = 'stream_engine';
    case None = 'none';

    public function getValue(): string
    {
        return $this->value;
    }

    public function getName(): string
    {
        return match ($this) {
            self::StreamEngine => 'AzuraCast Engine',
            self::None => 'Disabled',
        };
    }

    /**
     * @return class-string<AbstractLocalAdapter&BackendInterface>|null Every case here (besides
     *         None) both extends AbstractLocalAdapter and implements BackendInterface -- see
     *         Adapters::getBackendAdapter()'s matching intersection return type.
     */
    public function getClass(): ?string
    {
        return match ($this) {
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
        return self::StreamEngine;
    }
}
