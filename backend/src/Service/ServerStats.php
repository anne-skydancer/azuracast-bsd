<?php

declare(strict_types=1);

namespace App\Service;

use App\Service\ServerStats\CpuData;
use App\Service\ServerStats\MemoryData;
use App\Service\ServerStats\NetworkData;
use App\Service\ServerStats\NetworkData\Received;
use App\Service\ServerStats\NetworkData\Transmitted;
use Brick\Math\BigDecimal;
use Brick\Math\BigInteger;

final class ServerStats
{
    /**
     * @return CpuData[]
     */
    public static function getCurrentLoad(): array
    {
        if (self::isFreeBsd()) {
            return self::getCurrentLoadFreeBsd();
        }

        $cpuStatsRaw = file('/proc/stat', FILE_IGNORE_NEW_LINES) ?: [];

        $cpuCoreData = [];

        foreach ($cpuStatsRaw as $statLine) {
            $lineData = preg_split('/\s+/', $statLine) ?: [];
            $lineName = array_shift($lineData) ?? '';

            if ($lineName === 'cpu') {
                $cpuCoreData[] = CpuData::fromCoreData('total', $lineData);
            } elseif (str_starts_with($lineName, 'cpu')) {
                $cpuCoreData[] = CpuData::fromCoreData($lineName, $lineData);
            }
        }

        return $cpuCoreData;
    }

    public static function calculateCpuDelta(CpuData $current, CpuData $previous): CpuData
    {
        $name = $current->name;

        $user = $current->user - $previous->user;
        $nice = $current->nice - $previous->nice;
        $system = $current->system - $previous->system;
        $idle = $current->idle - $previous->idle;

        $iowait = null;
        if ($current->iowait !== null && $previous->iowait !== null) {
            $iowait = $current->iowait - $previous->iowait;
        }

        $irq = null;
        if ($current->irq !== null && $previous->irq !== null) {
            $irq = $current->irq - $previous->irq;
        }

        $softirq = null;
        if ($current->softirq !== null && $previous->softirq !== null) {
            $softirq = $current->softirq - $previous->softirq;
        }

        $steal = null;
        if ($current->steal !== null && $previous->steal !== null) {
            $steal = $current->steal - $previous->steal;
        }

        $guest = null;
        if ($current->guest !== null && $previous->guest !== null) {
            $guest = $current->guest - $previous->guest;
        }

        return new CpuData(
            $name,
            true,
            $user,
            $nice,
            $system,
            $idle,
            $iowait,
            $irq,
            $softirq,
            $steal,
            $guest
        );
    }

    public static function getMemoryUsage(): MemoryData
    {
        if (self::isFreeBsd()) {
            return self::getMemoryUsageFreeBsd();
        }

        $meminfoRaw = file('/proc/meminfo', FILE_IGNORE_NEW_LINES) ?: [];
        $meminfo = [];

        foreach ($meminfoRaw as $line) {
            if (!str_contains($line, ':')) {
                continue;
            }

            [$key, $val] = explode(':', $line);
            $meminfo[$key] = trim($val);
        }

        return MemoryData::fromMeminfo($meminfo);
    }

    public static function getNetworkUsage(): array
    {
        if (self::isFreeBsd()) {
            return self::getNetworkUsageFreeBsd();
        }

        $networkRaw = file('/proc/net/dev', FILE_IGNORE_NEW_LINES) ?: [];
        $currentTimestamp = microtime(true);
        $interfaces = [];

        foreach ($networkRaw as $lineNumber => $line) {
            if ($lineNumber <= 1) {
                continue;
            }

            [$interfaceName, $interfaceData] = explode(':', $line);
            $interfaceName = trim($interfaceName);
            $interfaceData = preg_split('/\s+/', trim($interfaceData)) ?: [];

            $interfaces[] = NetworkData::fromInterfaceData(
                $interfaceName,
                BigDecimal::of(sprintf('%F', $currentTimestamp)),
                $interfaceData
            );
        }

        return $interfaces;
    }

    public static function calculateNetworkDelta(NetworkData $current, NetworkData $previous): NetworkData
    {
        $interfaceName = $current->interfaceName;

        $received = self::calculateReceivedDelta($current->received, $previous->received);
        $transmitted = self::calculateTransmittedDelta($current->transmitted, $previous->transmitted);

        return new NetworkData(
            $interfaceName,
            $current->time->minus($previous->time),
            $received,
            $transmitted,
            true
        );
    }

    public static function calculateReceivedDelta(Received $current, Received $previous): Received
    {
        return new Received(
            $current->bytes->minus($previous->bytes),
            $current->packets->minus($previous->packets),
            $current->errs->minus($previous->errs),
            $current->drop->minus($previous->drop),
            $current->fifo->minus($previous->fifo),
            $current->frame->minus($previous->frame),
            $current->compressed->minus($previous->compressed),
            $current->multicast->minus($previous->multicast)
        );
    }

    public static function calculateTransmittedDelta(Transmitted $current, Transmitted $previous): Transmitted
    {
        return new Transmitted(
            $current->bytes->minus($previous->bytes),
            $current->packets->minus($previous->packets),
            $current->errs->minus($previous->errs),
            $current->drop->minus($previous->drop),
            $current->fifo->minus($previous->fifo),
            $current->colls->minus($previous->colls),
            $current->carrier->minus($previous->carrier),
            $current->compressed->minus($previous->compressed)
        );
    }

    // ------------------------------------------------------------------
    // FreeBSD implementations. The Linux code above reads procfs, which
    // does not exist on FreeBSD (and FreeBSD's optional procfs doesn't
    // provide these files in Linux format anyway) -- on the FreeBSD jail
    // deployment the equivalents come from sysctl(8) and netstat(8).
    // ------------------------------------------------------------------

    private static function isFreeBsd(): bool
    {
        return 'BSD' === PHP_OS_FAMILY;
    }

    /**
     * @return int[]
     */
    private static function sysctlIntList(string $name): array
    {
        $raw = shell_exec('/sbin/sysctl -n ' . escapeshellarg($name)) ?: '';
        $parts = preg_split('/\s+/', trim($raw)) ?: [];

        $values = [];
        foreach ($parts as $part) {
            if (is_numeric($part)) {
                $values[] = (int)$part;
            }
        }
        return $values;
    }

    private static function sysctlInt(string $name): int
    {
        $values = self::sysctlIntList($name);
        return $values[0] ?? 0;
    }

    /**
     * kern.cp_time (aggregate) and kern.cp_times (per-core) are lists of
     * scheduler ticks in the fixed order: user, nice, sys, intr, idle.
     *
     * @return CpuData[]
     */
    private static function getCurrentLoadFreeBsd(): array
    {
        $cpuCoreData = [];

        $total = self::sysctlIntList('kern.cp_time');
        if (count($total) >= 5) {
            $cpuCoreData[] = new CpuData(
                'total',
                false,
                $total[0],
                $total[1],
                $total[2],
                $total[4],
                null,
                $total[3]
            );
        }

        $perCore = self::sysctlIntList('kern.cp_times');
        $coreCount = intdiv(count($perCore), 5);
        for ($i = 0; $i < $coreCount; $i++) {
            $core = array_slice($perCore, $i * 5, 5);
            $cpuCoreData[] = new CpuData(
                'cpu' . $i,
                false,
                $core[0],
                $core[1],
                $core[2],
                $core[4],
                null,
                $core[3]
            );
        }

        return $cpuCoreData;
    }

    private static function getMemoryUsageFreeBsd(): MemoryData
    {
        $pageSize = self::sysctlInt('hw.pagesize');

        $memTotal = BigInteger::of(self::sysctlInt('vm.stats.vm.v_page_count'))->multipliedBy($pageSize);
        $memFree = BigInteger::of(self::sysctlInt('vm.stats.vm.v_free_count'))->multipliedBy($pageSize);

        // Inactive + laundry pages are reclaimable page cache -- the
        // closest FreeBSD analog to Linux meminfo's "Cached".
        $cached = BigInteger::of(
            self::sysctlInt('vm.stats.vm.v_inactive_count') + self::sysctlInt('vm.stats.vm.v_laundry_count')
        )->multipliedBy($pageSize);

        $buffers = BigInteger::of(self::sysctlInt('vfs.bufspace'));

        [$swapTotal, $swapFree] = self::getSwapUsageFreeBsd();

        return new MemoryData(
            $memTotal,
            $memFree,
            $buffers,
            $cached,
            BigInteger::zero(),
            BigInteger::zero(),
            $swapTotal,
            $swapFree
        );
    }

    /**
     * Sums swapinfo(8)'s per-device rows (KB columns: name, total, used,
     * avail, capacity). Inside a jail swapinfo may be unavailable or
     * empty; both degrade to zeroes rather than erroring.
     *
     * @return array{BigInteger, BigInteger}
     */
    private static function getSwapUsageFreeBsd(): array
    {
        $raw = shell_exec('/usr/sbin/swapinfo -k 2>/dev/null') ?: '';

        $totalKb = 0;
        $usedKb = 0;
        foreach (explode("\n", trim($raw)) as $line) {
            $cols = preg_split('/\s+/', trim($line)) ?: [];
            if (count($cols) >= 3 && is_numeric($cols[1]) && is_numeric($cols[2])) {
                $totalKb += (int)$cols[1];
                $usedKb += (int)$cols[2];
            }
        }

        return [
            BigInteger::of($totalKb)->multipliedBy(1024),
            BigInteger::of($totalKb - $usedKb)->multipliedBy(1024),
        ];
    }

    /**
     * Parses netstat -ibn's link-layer rows (Network column "<Link#N>"):
     *   Name Mtu Network [Address] Ipkts Ierrs Idrop Ibytes Opkts Oerrs Obytes Coll
     * The Address column is absent for interfaces with no MAC (e.g. lo0),
     * so the data offset is probed rather than assumed. Counters FreeBSD
     * doesn't track per-interface (fifo/frame/compressed/multicast/
     * carrier, tx drops) are reported as zero.
     *
     * @return NetworkData[]
     */
    private static function getNetworkUsageFreeBsd(): array
    {
        $raw = shell_exec('/usr/bin/netstat -ibn') ?: '';
        $currentTimestamp = microtime(true);
        $time = BigDecimal::of(sprintf('%F', $currentTimestamp));

        $interfaces = [];
        foreach (explode("\n", trim($raw)) as $line) {
            $cols = preg_split('/\s+/', trim($line)) ?: [];
            if (!isset($cols[2]) || !str_starts_with($cols[2], '<Link')) {
                continue;
            }

            // The Address column's contents vary by interface type (a MAC
            // for ethernet/epair, the literal interface name for lo0,
            // absent entirely for some tunnels) -- so rather than guess
            // its shape, skip to the first numeric column: that's Ipkts.
            $colCount = count($cols);
            $dataStart = 3;
            while ($dataStart < $colCount && !is_numeric($cols[$dataStart])) {
                $dataStart++;
            }
            if ($colCount < $dataStart + 8) {
                continue;
            }

            [$inPkts, $inErrs, $inDrop, $inBytes, $outPkts, $outErrs, $outBytes, $colls] =
                array_slice($cols, $dataStart, 8);

            $interfaces[] = NetworkData::fromInterfaceData(
                $cols[0],
                $time,
                [
                    $inBytes, $inPkts, $inErrs, $inDrop, 0, 0, 0, 0,
                    $outBytes, $outPkts, $outErrs, 0, 0, $colls, 0, 0,
                ]
            );
        }

        return $interfaces;
    }
}
