#!/bin/sh
#
# freebsd/mariadb/00-install.sh
#
# Installs and enables MariaDB server inside the `mariadb` jail
# (10.8.0.100 / mariadb.amc202d.lan). This jail runs ONLY MariaDB —
# no PHP, no nginx, no Redis/Valkey.
#
# Source-of-truth version note:
#   The Docker build (Dockerfile, `FROM mariadb:lts-noble AS mariadb`) only
#   uses the upstream `mariadb:lts-noble` image to lift out `healthcheck.sh`
#   and `docker-entrypoint.sh` — it does NOT set the actual server version.
#   The real server version is pinned in util/docker/mariadb/setup/mariadb.sh:
#     curl -LsS https://r.mariadb.com/downloads/mariadb_repo_setup | bash -s -- \
#       --mariadb-server-version=11.8.3 --skip-maxscale
#   i.e. MariaDB 11.8.x (the 11.8 LTS branch) is what AzuraCast actually ships.
#   (Note: util/docker/mariadb/mariadb/db.sql's dump header says
#   "Server version 11.4.4-MariaDB-deb12-log" — that's just stale metadata
#   left over from whenever that schema dump was captured; it does not
#   override the 11.8.3 pin in setup/mariadb.sh.)
#
#   FreeBSD ports/pkg names MariaDB packages "mariadbNNN-server" per major.minor
#   branch. The 11.8 branch is packaged as `mariadb118-server` (confirmed via
#   FreshPorts: databases/mariadb118-server, currently 11.8.8 on the latest
#   quarterly branch) — this is the closest available match to the 11.8.3
#   pinned upstream, so that's what we install below.

set -e

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)

. "${SCRIPT_DIR}/../env.conf"

pkg install -y mariadb118-server

# Enable the service to start at boot. Do NOT start it here — the
# 10-provision-db.sh script starts it explicitly on first run so it can
# perform one-time database/user provisioning before regular startup.
#
# NOTE: FreeBSD's mariadbNNN-server ports (including mariadb118-server)
# still ship the rc.d script under the legacy name "mysql-server" and use
# the rcvar "mysql_enable" (NOT "mariadb_enable") for backward
# compatibility with older mysql-server ports. Verified against FreshPorts'
# databases/mariadb118-server "USE_RC_SUBR" listing. `10-provision-db.sh`
# in this same directory uses `service mysql-server start` accordingly.
sysrc mysql_enable=YES

# --- Render templates from freebsd/env.conf ---------------------------------
# my.cnf.tmpl (bind-address templated from MARIADB_JAIL_IP and
# MARIADB_JAIL_IP6, both sourced from env.conf above) is rendered
# directly to the location the mariadb118-server package reads config
# fragments from. Listing both makes MariaDB dual-stack bound so webapp
# can reach it over either address family.
mkdir -p /usr/local/etc/mysql/conf.d
sed -e "s|@@MARIADB_JAIL_IP@@|${MARIADB_JAIL_IP}|g" \
    -e "s|@@MARIADB_JAIL_IP6@@|${MARIADB_JAIL_IP6}|g" \
    "${SCRIPT_DIR}/my.cnf.tmpl" >/usr/local/etc/mysql/conf.d/azuracast.cnf

# Network-level access control (pf or otherwise) is out of scope for this
# project -- restricting which hosts can reach MARIADB_JAIL_IP:MARIADB_PORT
# is left entirely to your own firewall policy.
