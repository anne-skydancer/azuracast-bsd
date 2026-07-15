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
# Mirrors freebsd/webapp/20-supervisor.sh's installation method exactly
# (pip install supervisor), for consistency across the whole freebsd/ tree.
# See that file's header comment for the full Option A/B rationale:
#
# Option A (used below): pip install supervisor.
#   Docker's own util/docker/supervisor/setup/supervisor.sh does exactly
#   this (`pip3 install --no-cache-dir --break-system-packages
#   setuptools supervisor git+https://.../supervisor-stdout`), so this
#   mirrors it most faithfully. Requires py-pip.
#
# Option B (alternative, not used below): `pkg install -y py311-supervisor`
#   FreeBSD ports carries supervisor as a per-Python-version port
#   (sysutils/py-supervisor, generated as e.g. py311-supervisor). This
#   avoids pip entirely if you'd rather stay on pure pkg. Swap the pip
#   install line below for this if preferred -- not verified against a
#   specific Python/FreeBSD-release combination as part of this change.

set -e

pkg install -y python311 py311-pip

python3.11 -m pip install --no-cache-dir supervisor

# supervisor-stdout is omitted here for the same reason webapp/
# 20-supervisor.sh omits it: meaningless outside a container, and this
# jail's Icecast process already presumably logs to its own file(s) (see
# README.md's "Shared mount" section for where those land).

mkdir -p /usr/local/etc/supervisor.d
mkdir -p /var/log/azuracast
mkdir -p /var/run

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
