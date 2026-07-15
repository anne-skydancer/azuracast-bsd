<?php

declare(strict_types=1);

namespace App\VueComponent;

use App\Entity\Api\Admin\Vue\StationsFormProps;
use App\Http\ServerRequest;
use App\Radio\StereoTool;
use App\Utilities\Time;
use Symfony\Component\Intl\Countries;

final readonly class StationFormComponent implements VueComponentInterface
{
    public function getProps(ServerRequest $request): StationsFormProps
    {
        return new StationsFormProps(
            timezones: Time::getTimezones(),
            countries: Countries::getNames(),
            isStereoToolInstalled: StereoTool::isInstalled()
        );
    }
}
