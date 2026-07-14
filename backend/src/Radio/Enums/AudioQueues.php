<?php

declare(strict_types=1);

namespace App\Radio\Enums;

enum AudioQueues: string
{
    case Requests = 'requests';
    case Interrupting = 'interrupting_requests';

    public static function default(): self
    {
        return self::Requests;
    }
}
