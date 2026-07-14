#!/bin/sh
#
# freebsd/webapp/configure-db.sh
#
# Interactive first-install database configuration for the `webapp` jail.
# Run this once, after the AzuraCast application itself has been deployed
# (git clone / composer install / frontend build) but before starting the
# `azuracast` service for the first time.
#
# Writes (or updates, preserving any other existing keys) the MYSQL_*
# values in <AZURACAST_PATH>/azuracast.env -- the exact file AzuraCast's
# own Environment class reads (backend/src/Installer/EnvFiles/
# AzuraCastEnvFile::buildPathFromBase(), which resolves to
# "<app base dir>/azuracast.env").
#
# Two paths, chosen interactively:
#   1. You already have a MariaDB/MySQL-compatible server somewhere
#      (this host, another jail, a remote box) -- you supply its
#      address, port, database name, user, and password directly.
#   2. You don't -- this falls back to the default topology documented
#      in freebsd/mariadb/ (a dedicated `mariadb` jail at 10.8.0.100),
#      and only prompts for the database name/user/password (host/port
#      are the known defaults for that jail).
#
# Either way, nothing here provisions a server for you -- if you pick
# option 2 and haven't already run freebsd/mariadb/00-install.sh +
# 10-provision-db.sh with matching values, do that first (or right
# after) using the same name/user/password you enter here.

set -e

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)

. "${SCRIPT_DIR}/../env.conf"

: ${AZURACAST_PATH:="/usr/local/www/azuracast"}

ENV_FILE="${AZURACAST_PATH}/azuracast.env"

# --- helpers -----------------------------------------------------------------

prompt() {
    # prompt "Question text" "default value" -> echoes the answer
    _q="$1"
    _default="$2"
    if [ -n "$_default" ]; then
        printf '%s [%s]: ' "$_q" "$_default" 1>&2
    else
        printf '%s: ' "$_q" 1>&2
    fi
    read -r _answer
    if [ -z "$_answer" ]; then
        _answer="$_default"
    fi
    printf '%s' "$_answer"
}

prompt_hidden() {
    # prompt_hidden "Question text" -> echoes the answer, without echoing
    # keystrokes to the terminal. /bin/sh on FreeBSD has no `read -s`, so
    # this toggles tty echo directly.
    _q="$1"
    printf '%s: ' "$_q" 1>&2
    stty -echo 2>/dev/null || true
    read -r _answer
    stty echo 2>/dev/null || true
    printf '\n' 1>&2
    printf '%s' "$_answer"
}

set_env_var() {
    # set_env_var KEY VALUE -- upserts KEY=VALUE into $ENV_FILE, preserving
    # every other line already in the file (creates the file if missing).
    _key="$1"
    _value="$2"

    touch "$ENV_FILE"

    if grep -q "^${_key}=" "$ENV_FILE" 2>/dev/null; then
        # Escape & and \ for sed's replacement text, then substitute in place.
        _escaped=$(printf '%s' "$_value" | sed -e 's/[\&]/\\&/g')
        sed -i '' "s#^${_key}=.*#${_key}=${_escaped}#" "$ENV_FILE"
    else
        printf '%s=%s\n' "$_key" "$_value" >>"$ENV_FILE"
    fi
}

# --- main ---------------------------------------------------------------------

echo "=== AzuraCast database configuration ==="
echo "Writing to: ${ENV_FILE}"
echo

_has_existing=$(prompt "Do you already have a MariaDB/MySQL-compatible database server set up? [y/N]" "N")

case "$_has_existing" in
    [yY]*)
        echo
        echo "Enter the connection details for your existing database server."
        _db_host=$(prompt "Database host/address" "")
        while [ -z "$_db_host" ]; do
            echo "Host is required." 1>&2
            _db_host=$(prompt "Database host/address" "")
        done
        _db_port=$(prompt "Database port" "3306")
        _db_name=$(prompt "Database name" "${AZURACAST_DB_NAME_DEFAULT}")
        _db_user=$(prompt "Database user" "${AZURACAST_DB_USER_DEFAULT}")
        _db_password=$(prompt_hidden "Database password")
        while [ -z "$_db_password" ]; do
            echo "Password is required." 1>&2
            _db_password=$(prompt_hidden "Database password")
        done
        ;;
    *)
        echo
        echo "Using the default topology: a dedicated 'mariadb' jail at ${MARIADB_JAIL_IP}:${MARIADB_PORT}"
        echo "(see freebsd/mariadb/README.md). If you haven't provisioned it yet, run"
        echo "freebsd/mariadb/00-install.sh and 10-provision-db.sh with matching values"
        echo "before starting the azuracast service."
        echo
        # Defaults to MariaDB's IPv4 address (MARIADB_JAIL_IP) -- MariaDB is
        # dual-stack bound (see freebsd/mariadb/my.cnf.tmpl), so
        # MARIADB_JAIL_IP6 from env.conf also works here and can be
        # substituted manually below if you prefer IPv6 for this internal
        # jail-to-jail link; IPv4 has no practical disadvantage over IPv6 on
        # this internal network, so no interactive prompt is offered for it.
        _db_host="${MARIADB_JAIL_IP}"
        _db_port="${MARIADB_PORT}"
        _db_name=$(prompt "Database name" "${AZURACAST_DB_NAME_DEFAULT}")
        _db_user=$(prompt "Database user" "${AZURACAST_DB_USER_DEFAULT}")
        _db_password=$(prompt_hidden "Database password")
        while [ -z "$_db_password" ]; do
            echo "Password is required." 1>&2
            _db_password=$(prompt_hidden "Database password")
        done
        ;;
esac

set_env_var "MYSQL_HOST" "$_db_host"
set_env_var "MYSQL_PORT" "$_db_port"
set_env_var "MYSQL_DATABASE" "$_db_name"
set_env_var "MYSQL_USER" "$_db_user"
set_env_var "MYSQL_PASSWORD" "$_db_password"

chown azuracast:azuracast "$ENV_FILE" 2>/dev/null || true
chmod 600 "$ENV_FILE"

echo
echo "Done. Database settings written to ${ENV_FILE} (mode 600, owned by azuracast)."
echo "MYSQL_HOST=${_db_host}"
echo "MYSQL_PORT=${_db_port}"
echo "MYSQL_DATABASE=${_db_name}"
echo "MYSQL_USER=${_db_user}"
echo "MYSQL_PASSWORD=(hidden)"
echo
echo "If you chose the default 'mariadb' jail and haven't provisioned it yet, run"
echo "  export AZURACAST_DB_NAME='${_db_name}'"
echo "  export AZURACAST_DB_USER='${_db_user}'"
echo "  export AZURACAST_DB_PASSWORD='<the same password you just entered>'"
echo "  sh freebsd/mariadb/10-provision-db.sh"
echo "on the mariadb jail now, using the exact same name/user/password."
