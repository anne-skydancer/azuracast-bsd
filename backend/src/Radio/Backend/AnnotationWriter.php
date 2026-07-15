<?php

declare(strict_types=1);

namespace App\Radio\Backend;

use App\Event\Radio\AnnotateNextSong;
use App\Utilities\Types;

/**
 * Builds the `annotate:key="val",...:path` string format sent to the streaming
 * engine as the `nextsong`/`cp` callback response (SPEC.md D.1/D.6) and parsed
 * back by the engine's own annotation parser (`engine/src/annotate.rs`). This
 * on-wire format predates the Rust engine (it was Liquidsoap's native
 * `annotate:` syntax) but is deliberately kept as the shared contract between
 * PHP and the engine rather than replaced.
 */
final class AnnotationWriter
{
    /**
     * Given a value, convert it into an annotation-friendly quoted string.
     */
    public static function annotateValue(string|int|float|bool $dataVal, bool $preserveType = false): string
    {
        if ($preserveType) {
            $strVal = Types::string($dataVal);
        } else {
            $strVal = match (true) {
                'true' === $dataVal || 'false' === $dataVal => $dataVal,
                is_bool($dataVal) => Types::bool($dataVal, false, true) ? 'true' : 'false',
                is_numeric($dataVal) && !is_int($dataVal) => self::toFloat($dataVal),
                default => Types::string($dataVal)
            };
        }

        $strVal = mb_convert_encoding($strVal, 'UTF-8');

        return str_replace(['"', "\n", "\t", "\r"], ['\"', '', '', ''], $strVal);
    }

    public static function toFloat(float|int|string $number, int $decimals = 2): string
    {
        return number_format(
            Types::float($number),
            $decimals,
            '.',
            ''
        );
    }

    public static function annotateArray(array $values): string
    {
        $values = array_filter(
            $values,
            fn(string|int|float|bool|null $val, string $key): bool => $val !== null
                && in_array($key, AnnotateNextSong::ALLOWED_ANNOTATIONS, true),
            ARRAY_FILTER_USE_BOTH
        );

        $annotations = [];
        foreach ($values as $key => $val) {
            $annotatedVal = self::annotateValue(
                $val,
                in_array($key, AnnotateNextSong::ALWAYS_STRING_ANNOTATIONS, true)
            );

            $annotations[] = $key . '="' . $annotatedVal . '"';
        }

        return implode(',', $annotations);
    }
}
