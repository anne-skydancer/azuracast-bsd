<?php

declare(strict_types=1);

namespace App\Radio\Backend;

interface OutputtableInterface
{
    public function getOutputtableSource(): ?OutputtableSource;
}
