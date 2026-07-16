#!/bin/sh
#
# freebsd/icecast/watchdog.sh
#
# Wedge watchdog for Icecast running under supervisord in a station's
# Icecast jail. Install INSIDE the jail (e.g. /usr/local/sbin/
# icecast-watchdog) and run it from the jail's root crontab every minute:
#
#   * * * * * /usr/local/sbin/icecast-watchdog >> /var/log/azuracast/watchdog.log 2>&1
#
# Why this exists -- confirmed twice on a real install (2026-07): the
# FreeBSD port's Icecast 2.5 beta can wedge its HTTP request pipeline
# (process alive, TCP accepted, but requests never answered -- empty
# responses to status-json.xsl and held-forever SOURCE handshakes) after
# abnormal events: once following an abrupt source disconnect, once
# following a config reload triggered by AzuraCast's SSL-renewal hook.
# Because the process stays alive, supervisord's autorestart never fires;
# this watchdog probes actual HTTP behavior instead. Two consecutive
# failed probes (>= ~2 minutes of catatonia) trigger one supervisord
# restart of the frontend program; the streaming engine reconnects on its
# own within seconds (its handshake timeout + reconnect backoff), so a
# watchdog restart costs a brief dropout rather than an outage that lasts
# until a human notices the silence.
#
# Configuration (override via environment or edit here):
#   WATCHDOG_URL       -- probe URL; localhost:PORT from inside the jail.
#   WATCHDOG_PROGRAM   -- supervisord program name for this station's
#                         frontend (station_<id>_frontend).
#   WATCHDOG_STATE     -- failure-counter file.

set -u

WATCHDOG_URL="${WATCHDOG_URL:-http://127.0.0.1:8000/status-json.xsl}"
WATCHDOG_PROGRAM="${WATCHDOG_PROGRAM:-station_1_frontend}"
WATCHDOG_STATE="${WATCHDOG_STATE:-/var/run/icecast-watchdog.failcount}"
SUPERVISORCTL="/usr/local/bin/supervisorctl"
SUPERVISORD_CONF="/usr/local/etc/supervisord.conf"

# Only meaningful when the program is actually supposed to be running --
# if supervisord reports anything but RUNNING (stopped by an operator,
# FATAL from a real crash, supervisord itself down), stay out of the way
# and let supervisord/the operator own the situation.
prog_state=$("$SUPERVISORCTL" -c "$SUPERVISORD_CONF" status "$WATCHDOG_PROGRAM" 2>/dev/null | awk '{print $2}')
if [ "$prog_state" != "RUNNING" ]; then
    rm -f "$WATCHDOG_STATE"
    exit 0
fi

# A wedged Icecast accepts the TCP connection and then never answers, so
# the timeout matters as much as the HTTP status. fetch(1) rather than
# curl: it's in the FreeBSD base system, so this script has no package
# dependencies inside a minimal Icecast jail.
if fetch -q -T 10 -o /dev/null "$WATCHDOG_URL" 2>/dev/null; then
    rm -f "$WATCHDOG_STATE"
    exit 0
fi

fails=$(cat "$WATCHDOG_STATE" 2>/dev/null || echo 0)
fails=$((fails + 1))

if [ "$fails" -lt 2 ]; then
    echo "$fails" > "$WATCHDOG_STATE"
    echo "$(date -u '+%Y-%m-%dT%H:%M:%SZ') probe failed (${fails}/2) -- will restart on next consecutive failure"
    exit 0
fi

rm -f "$WATCHDOG_STATE"
echo "$(date -u '+%Y-%m-%dT%H:%M:%SZ') probe failed twice consecutively -- restarting ${WATCHDOG_PROGRAM}"
"$SUPERVISORCTL" -c "$SUPERVISORD_CONF" restart "$WATCHDOG_PROGRAM"
