#!/bin/sh
#
# freebsd/generate-jail-conf.sh
#
# Renders freebsd/jail.conf.d/mariadb.conf and freebsd/jail.conf.d/webapp.conf
# from their freebsd/jail.conf.d/*.conf.tmpl counterparts, substituting every
# @@TOKEN@@ placeholder with the matching shell variable from
# freebsd/env.conf.
#
# Run this once after editing env.conf to match your own jail/network
# layout (jail names, hostnames, paths, epair interfaces, IPv4/IPv6
# addresses, routes) -- and again any time you change a value there. The
# two rendered *.conf files are committed to the repo as the ready-to-use
# reference deployment, but they are generated output: hand edits to them
# are overwritten the next time this script runs. Edit the *.conf.tmpl
# files (structure) or freebsd/env.conf (values) instead.
#
# Usage:
#   sh freebsd/generate-jail-conf.sh

set -e

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)

if [ ! -f "${SCRIPT_DIR}/env.conf" ]; then
    echo "ERROR: freebsd/env.conf not found." >&2
    echo "Copy freebsd/env.conf.example to freebsd/env.conf and edit it to" >&2
    echo "match your own jail/network layout first (see freebsd/README.md)." >&2
    exit 1
fi

. "${SCRIPT_DIR}/env.conf"

# render <template-file> -- substitutes every known @@TOKEN@@ in
# <template-file> and prints the result on stdout.
render() {
    sed \
        -e "s|@@MARIADB_JAIL_NAME@@|${MARIADB_JAIL_NAME}|g" \
        -e "s|@@MARIADB_JAIL_HOSTNAME@@|${MARIADB_JAIL_HOSTNAME}|g" \
        -e "s|@@MARIADB_JAIL_PATH@@|${MARIADB_JAIL_PATH}|g" \
        -e "s|@@MARIADB_JAIL_EPAIR@@|${MARIADB_JAIL_EPAIR}|g" \
        -e "s|@@MARIADB_JAIL_IP@@|${MARIADB_JAIL_IP}|g" \
        -e "s|@@MARIADB_JAIL_NETMASK@@|${MARIADB_JAIL_NETMASK}|g" \
        -e "s|@@MARIADB_JAIL_IP6@@|${MARIADB_JAIL_IP6}|g" \
        -e "s|@@MARIADB_JAIL_IP6_PREFIX@@|${MARIADB_JAIL_IP6_PREFIX}|g" \
        -e "s|@@MARIADB_PORT@@|${MARIADB_PORT}|g" \
        -e "s|@@WEBAPP_JAIL_NAME@@|${WEBAPP_JAIL_NAME}|g" \
        -e "s|@@WEBAPP_JAIL_HOSTNAME@@|${WEBAPP_JAIL_HOSTNAME}|g" \
        -e "s|@@WEBAPP_JAIL_PATH@@|${WEBAPP_JAIL_PATH}|g" \
        -e "s|@@WEBAPP_JAIL_EPAIR@@|${WEBAPP_JAIL_EPAIR}|g" \
        -e "s|@@WEBAPP_JAIL_IP@@|${WEBAPP_JAIL_IP}|g" \
        -e "s|@@WEBAPP_JAIL_NETMASK@@|${WEBAPP_JAIL_NETMASK}|g" \
        -e "s|@@WEBAPP_JAIL_IP6@@|${WEBAPP_JAIL_IP6}|g" \
        -e "s|@@WEBAPP_JAIL_IP6_PREFIX@@|${WEBAPP_JAIL_IP6_PREFIX}|g" \
        -e "s|@@INTEGRATED_JAIL_NAME@@|${INTEGRATED_JAIL_NAME}|g" \
        -e "s|@@INTEGRATED_JAIL_HOSTNAME@@|${INTEGRATED_JAIL_HOSTNAME}|g" \
        -e "s|@@INTEGRATED_JAIL_PATH@@|${INTEGRATED_JAIL_PATH}|g" \
        -e "s|@@INTEGRATED_JAIL_EPAIR@@|${INTEGRATED_JAIL_EPAIR}|g" \
        -e "s|@@INTEGRATED_JAIL_IP@@|${INTEGRATED_JAIL_IP}|g" \
        -e "s|@@INTEGRATED_JAIL_NETMASK@@|${INTEGRATED_JAIL_NETMASK}|g" \
        -e "s|@@INTEGRATED_JAIL_IP6@@|${INTEGRATED_JAIL_IP6}|g" \
        -e "s|@@INTEGRATED_JAIL_IP6_PREFIX@@|${INTEGRATED_JAIL_IP6_PREFIX}|g" \
        -e "s|@@VM_PUBLIC_BRIDGE@@|${VM_PUBLIC_BRIDGE}|g" \
        -e "s|@@DEFAULT_ROUTE_V4@@|${DEFAULT_ROUTE_V4}|g" \
        -e "s|@@DEFAULT_ROUTE_V6_GW@@|${DEFAULT_ROUTE_V6_GW}|g" \
        "$1"
}

# generate_one <name> -- renders jail.conf.d/<name>.conf.tmpl to
# jail.conf.d/<name>.conf, with a generated-file banner prepended.
generate_one() {
    _name="$1"
    _tmpl="${SCRIPT_DIR}/jail.conf.d/${_name}.conf.tmpl"
    _out="${SCRIPT_DIR}/jail.conf.d/${_name}.conf"

    {
        echo "# GENERATED FILE -- do not hand-edit."
        echo "# Rendered from jail.conf.d/${_name}.conf.tmpl + env.conf by"
        echo "# freebsd/generate-jail-conf.sh. Re-running that script overwrites"
        echo "# any changes made directly to this file."
        echo "#"
        render "${_tmpl}"
    } >"${_out}"

    echo "Wrote ${_out}"
}

generate_one mariadb
generate_one webapp
# The integrated (all-in-one-jail) alternative topology -- see
# freebsd/install.sh. Rendered unconditionally (it's cheap and keeps the
# reference output complete); only install the stanza for the topology
# you actually chose.
generate_one integrated
