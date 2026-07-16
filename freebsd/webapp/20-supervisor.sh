#!/bin/sh
#
# freebsd/webapp/20-supervisor.sh
#
# Installs supervisord (process manager for nginx/php-fpm/centrifugo/
# sftpgo, plus the dynamically-generated per-station backend/frontend
# programs that are out of scope for this change) and lays out the
# directory structure + socket path its PHP client expects.
#
# Source-of-truth for the socket path:
#   util/docker/supervisor/supervisor/supervisord.conf uses:
#     [unix_http_server]
#     file = /var/run/supervisor.sock
#   and backend/config/services.php (line ~545) hardcodes the PHP
#   client to that exact path:
#     CURLOPT_UNIX_SOCKET_PATH => '/var/run/supervisor.sock',
#   Since we are not writing/modifying PHP application code, this path
#   MUST stay exactly `/var/run/supervisor.sock` on the FreeBSD box too —
#   do not relocate it (e.g. to /usr/local/var/run) without also
#   patching that PHP source.
#
# Option A (used below): pip install supervisor.
#   Docker's own util/docker/supervisor/setup/supervisor.sh does exactly
#   this (`pip3 install --no-cache-dir --break-system-packages
#   setuptools supervisor git+https://.../supervisor-stdout`), so this
#   mirrors it most faithfully. Requires py-pip.
#
# Option B (alternative, not used below): `pkg install -y py312-supervisor`
#   FreeBSD ports carries supervisor as a per-Python-version port
#   (sysutils/py-supervisor, generated as e.g. py312-supervisor for the
#   current default Python 3.12 -- confirmed on a real install 2026-07).
#   This avoids pip entirely, and ships a native rc.d service -- which is
#   exactly why freebsd/icecast/00-install-supervisor.sh DOES use it. It
#   is deliberately NOT used here: this jail's supervisord lifecycle is
#   owned by rc.d/azuracast (MariaDB-wait + migrations before start), and
#   an independently-enabled supervisord service alongside it means two
#   supervisords fighting over the same programs -- confirmed the hard
#   way on a real install.

set -e

pkg install -y python312 py312-pip

python3.12 -m pip install --no-cache-dir supervisor

# supervisor-stdout (used by Docker's [eventlistener:stdout] to fold
# per-program output into the container's stdout) is meaningless on a
# jail where every program logs to its own file already — omitted
# on purpose. See supervisord.conf in this directory: logs go to
# /var/log/azuracast/*.log instead of /proc/1/fd/*.

mkdir -p /usr/local/etc/supervisor.d
mkdir -p /var/log/azuracast
mkdir -p /var/run

chown azuracast:azuracast /var/log/azuracast

echo "Installed: $(supervisord --version 2>&1)"
echo "Next: copy freebsd/webapp/supervisord.conf to /usr/local/etc/supervisord.conf"
echo "Then: cp freebsd/webapp/rc.d/azuracast /usr/local/etc/rc.d/azuracast && chmod +x same"
