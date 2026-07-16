#!/bin/sh
#
# freebsd/icecast/00-install-supervisor.sh
#
# Installs supervisord into an EXISTING, already-running Icecast jail so
# AzuraCast's PHP side can manage it remotely. Icecast itself is assumed to
# already be installed and running in this jail some other way -- this
# script does NOT install, configure, or touch Icecast in any way. Run it
# standalone, copied into (or run against, e.g. via `jexec`) whichever
# station's Icecast jail you're applying this template to -- see
# README.md in this directory for the full apply procedure.
#
# Unlike freebsd/webapp/20-supervisor.sh (which uses `pip install
# supervisor` to mirror Docker's own supervisor.sh), this installs the
# FreeBSD PORT (sysutils/py-supervisor) instead -- deliberately:
#
#   - The port ships a native rc.d service (/usr/local/etc/rc.d/supervisord,
#     enabled via `sysrc supervisord_enable=YES`), so supervisord starts at
#     jail boot with no rc.local hackery. Its default config path is
#     /usr/local/etc/supervisord.conf -- exactly where README.md's apply
#     procedure installs the rendered template, so no extra rcvar needed.
#     (sysutils/py-supervisor is a per-Python-flavor port; py312-supervisor
#     below matches the current default Python -- confirmed on a real
#     install 2026-07. Adjust the flavor prefix if your ports snapshot's
#     default Python differs: `pkg search supervisor` shows what's current.)
#   - The webapp jail does NOT want that service: its own rc.d/azuracast
#     owns supervisord's start there (MariaDB-wait + migrations first),
#     and an independently-enabled supervisord service alongside it means
#     two supervisords fighting over the same programs -- a failure mode
#     confirmed the hard way on a real install. Hence the two jails'
#     install scripts intentionally differ.
#
# If this jail previously had the pip-installed supervisor (an earlier
# revision of this script), remove it first so the port's files win --
# using whichever python the old install ran under (`head -1
# /usr/local/bin/supervisord` shows it in the shebang):
#   python3.12 -m pip uninstall -y supervisor

set -e

pkg install -y py312-supervisor

# Stock Icecast looks for /etc/mime.types at startup and logs
# "WARN fserve/fserve_recheck_mime_types Cannot open mime types file"
# on every start when it's absent (confirmed on a real install) --
# cosmetic, but noisy. FreeBSD ships the file at
# /usr/local/etc/mime.types via misc/mime-support; install it and link
# the path Icecast expects.
pkg install -y mime-support
if [ ! -e /etc/mime.types ]; then
    ln -s /usr/local/etc/mime.types /etc/mime.types
fi

sysrc supervisord_enable=YES

mkdir -p /usr/local/etc/supervisor.d
mkdir -p /var/log/azuracast
mkdir -p /var/run

# The azuracast user is NOT optional here: AzuraCast's generated
# supervisord.frontend.conf sets `user = azuracast` on the Icecast
# program, and supervisord refuses to even START if any loaded program
# names an unknown user ("Error: Invalid user name azuracast ...",
# confirmed on a real install). The uid/gid MUST match the webapp
# jail's azuracast user (uid 1001, from 00-packages.sh's pw useradd)
# because Icecast reads its config from -- and writes its pidfile/logs
# into -- the station config directory nullfs-mounted from the webapp
# jail, where everything is owned by that numeric uid.
#
# `-o` (allow non-unique uid) is load-bearing: a real Icecast jail very
# likely already has uid 1001 allocated -- confirmed on a real install,
# where the icecast package's own service user held icecast:1001. The
# name `azuracast` then becomes an alias for the same numeric uid, which
# is exactly what's needed (supervisord resolves the NAME, the nullfs
# mount cares about the NUMBER). The group likewise reuses gid 1001
# under whatever name already owns it, creating it only if genuinely
# absent.
if ! pw usershow azuracast >/dev/null 2>&1; then
    if ! pw groupshow -g 1001 >/dev/null 2>&1; then
        pw groupadd azuracast -g 1001
    fi
    pw useradd azuracast -u 1001 -o -g 1001 -d /nonexistent -s /usr/sbin/nologin -c "AzuraCast"
fi

# JUDGMENT CALL: unlike freebsd/webapp/20-supervisor.sh (which chowns this
# same directory to `azuracast:azuracast`, a user its own 00-packages.sh
# creates), this jail was NOT set up by this project -- it may not have an
# `azuracast` system user at all (Icecast may run as its own dedicated
# user, or as root). Left root-owned by default; if your Icecast jail does
# have a matching user and you'd rather mirror webapp's convention exactly,
# run `chown <that-user>:<that-group> /var/log/azuracast` yourself.

echo "Installed: $(supervisord --version 2>&1)"
echo "Next: sh freebsd/icecast/render-supervisord-conf.sh to render supervisord.conf.tmpl,"
echo "  then copy the rendered freebsd/icecast/supervisord.conf to /usr/local/etc/supervisord.conf"
echo "  on this jail (see freebsd/icecast/README.md)."
