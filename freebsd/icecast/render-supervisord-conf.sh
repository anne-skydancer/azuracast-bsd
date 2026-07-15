#!/bin/sh
#
# freebsd/icecast/render-supervisord-conf.sh
#
# Renders supervisord.conf.tmpl (in this directory) into a concrete
# supervisord.conf for ONE existing station's Icecast jail, substituting
# the @@ICECAST_JAIL_IP@@ / @@ICECAST_SUPERVISOR_PORT@@ /
# @@ICECAST_SUPERVISOR_USERNAME@@ / @@ICECAST_SUPERVISOR_PASSWORD@@ tokens
# -- the same sed-based @@TOKEN@@ substitution mechanism
# freebsd/mariadb/00-install.sh uses for my.cnf.tmpl (and
# freebsd/generate-jail-conf.sh uses for jail.conf.d/*.conf.tmpl).
#
# This directory is a TEMPLATE applied per-station Icecast jail, not a
# single fixed jail like freebsd/mariadb/ or freebsd/webapp/ -- so, unlike
# those, the values substituted here are NOT read from the shared
# freebsd/env.conf (that file is scoped to this host's fixed
# single-instance jails). Instead, this script takes them from -- in order
# of precedence, highest first:
#   1. Positional command-line arguments (see Usage below).
#   2. A small local .env-style file, icecast.env (see
#      icecast.env.example in this directory), sourced if present.
#   3. Plain environment variables already exported by the caller.
#
# Usage:
#   cp freebsd/icecast/icecast.env.example freebsd/icecast/icecast.env
#   # edit icecast.env for this specific station's Icecast jail
#   sh freebsd/icecast/render-supervisord-conf.sh
#
#   # or, without an env file, pass everything as arguments:
#   sh freebsd/icecast/render-supervisord-conf.sh <icecast-jail-ip> <supervisor-port> <supervisor-username> <supervisor-password>
#
# Writes freebsd/icecast/supervisord.conf (concrete, rendered,
# .gitignore'd) -- copy that file to /usr/local/etc/supervisord.conf on
# the target Icecast jail.

set -e

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)

# Positional args, captured before anything below reassigns $1.. via `.`.
ARG_IP="$1"
ARG_PORT="$2"
ARG_USERNAME="$3"
ARG_PASSWORD="$4"

ENV_FILE="${ICECAST_ENV_FILE:-${SCRIPT_DIR}/icecast.env}"

if [ -f "${ENV_FILE}" ]; then
    . "${ENV_FILE}"
else
    echo "No env file found at ${ENV_FILE} -- using CLI args / environment variables only." >&2
fi

# Positional args win over whatever icecast.env / the environment set.
if [ -n "${ARG_IP}" ]; then
    ICECAST_JAIL_IP="${ARG_IP}"
fi
if [ -n "${ARG_PORT}" ]; then
    ICECAST_SUPERVISOR_PORT="${ARG_PORT}"
fi
if [ -n "${ARG_USERNAME}" ]; then
    ICECAST_SUPERVISOR_USERNAME="${ARG_USERNAME}"
fi
if [ -n "${ARG_PASSWORD}" ]; then
    ICECAST_SUPERVISOR_PASSWORD="${ARG_PASSWORD}"
fi

# Default port matches Frontend\Icecast::DEFAULT_SUPERVISOR_PORT.
ICECAST_SUPERVISOR_PORT="${ICECAST_SUPERVISOR_PORT:-9002}"

: "${ICECAST_JAIL_IP:?Set ICECAST_JAIL_IP (this station's Icecast jail IPv4/IPv6 address) via a CLI arg, icecast.env, or the environment.}"
: "${ICECAST_SUPERVISOR_USERNAME:?Set ICECAST_SUPERVISOR_USERNAME via a CLI arg, icecast.env, or the environment.}"

# Same refuse-the-placeholder pattern freebsd/mariadb/00-install.sh's
# provisioning step uses for AZURACAST_DB_PASSWORD -- don't silently render
# a weak/default credential into a config that's reachable over TCP.
if [ -z "${ICECAST_SUPERVISOR_PASSWORD}" ] || [ "${ICECAST_SUPERVISOR_PASSWORD}" = "CHANGE_ME_SET_VIA_ENV" ]; then
    echo "ERROR: ICECAST_SUPERVISOR_PASSWORD is unset or left at its placeholder value." >&2
    echo "Set a strong password via a CLI arg, icecast.env, or the environment before rendering." >&2
    exit 1
fi

OUT="${SCRIPT_DIR}/supervisord.conf"

sed -e "s|@@ICECAST_JAIL_IP@@|${ICECAST_JAIL_IP}|g" \
    -e "s|@@ICECAST_SUPERVISOR_PORT@@|${ICECAST_SUPERVISOR_PORT}|g" \
    -e "s|@@ICECAST_SUPERVISOR_USERNAME@@|${ICECAST_SUPERVISOR_USERNAME}|g" \
    -e "s|@@ICECAST_SUPERVISOR_PASSWORD@@|${ICECAST_SUPERVISOR_PASSWORD}|g" \
    "${SCRIPT_DIR}/supervisord.conf.tmpl" >"${OUT}"

echo "Wrote ${OUT}"
echo "Next: copy this file to /usr/local/etc/supervisord.conf on the target Icecast jail,"
echo "then set matching host/supervisor_port/supervisor_username/supervisor_password"
echo "values on this station's frontend config in the AzuraCast admin UI."
