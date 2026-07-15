<?php

declare(strict_types=1);

namespace App\Radio\Backend;

interface EncodableInterface
{
    public function getEncodingFormat(): ?EncodingFormat;
}
