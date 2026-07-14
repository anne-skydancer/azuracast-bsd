#!/bin/sh
#
# freebsd/mariadb/10-provision-db.sh
#
# One-time provisioning for the `mariadb` jail (10.8.0.100). Run this ONCE,
# after freebsd/mariadb/00-install.sh and after my.cnf has been installed to
# /usr/local/etc/mysql/conf.d/azuracast.cnf.
#
# It will:
#   1. Run mysql_install_db (if the data directory doesn't exist yet).
#   2. Start mariadb via service(8) and wait for the socket to come up.
#   3. Create the `azuracast` database (utf8mb4 / utf8mb4_general_ci, matching
#      util/docker/mariadb/mariadb/db.sql and backend/config/services.php).
#   4. Create the `azuracast` DB user, scoped ONLY to
#      'azuracast'@'10.8.0.110' (the webapp jail's IP) — NOT '%' and NOT
#      'localhost'. The app connects over the network from a different jail,
#      so a localhost-scoped or wildcard-scoped grant would either not work
#      or would be far more permissive than necessary.
#   5. GRANT that user privileges on `azuracast`.* only.
#
# --- Setting the DB password ------------------------------------------------
# Do NOT hardcode a real password in this script. Set it via environment
# variable before running this script:
#
#   export AZURACAST_DB_PASSWORD='something-strong-and-unique'
#   sh freebsd/mariadb/10-provision-db.sh
#
# If AZURACAST_DB_PASSWORD is unset, this script falls back to the clearly
# fake placeholder CHANGE_ME_SET_VIA_ENV below and will refuse to proceed,
# to avoid silently provisioning a guessable/default credential.
#
# (For reference, AzuraCast's own Docker image defaults to user "azuracast"
# with password "azur4c457" for local/dev use only — see Dockerfile ENV and
# azuracast.sample.env. Do not reuse that dev password here.)
#
# --- Setting the DB name and user --------------------------------------------
# AzuraCast has no hardcoded assumption about the database name or user --
# they're plain config values read via MYSQL_DATABASE/MYSQL_USER
# (backend/src/Environment.php's getDatabaseSettings(), defaults 'azuracast'
# for both). Override either here if you want something other than the
# default, e.g.:
#
#   export AZURACAST_DB_NAME='my_custom_name'
#   export AZURACAST_DB_USER='my_custom_user'
#
# Whatever you pick here MUST match MYSQL_DATABASE/MYSQL_USER in the webapp
# jail's .env/azuracast.env exactly.

set -e

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)

. "${SCRIPT_DIR}/../env.conf"

DB_NAME="${AZURACAST_DB_NAME:-$AZURACAST_DB_NAME_DEFAULT}"
DB_USER="${AZURACAST_DB_USER:-$AZURACAST_DB_USER_DEFAULT}"
DB_USER_HOST="${WEBAPP_JAIL_IP}"
DB_PASSWORD="${AZURACAST_DB_PASSWORD:-CHANGE_ME_SET_VIA_ENV}"

if [ "$DB_PASSWORD" = "CHANGE_ME_SET_VIA_ENV" ]; then
    echo "ERROR: AZURACAST_DB_PASSWORD is not set." >&2
    echo "Set it before running this script, e.g.:" >&2
    echo "  export AZURACAST_DB_PASSWORD='your-strong-password-here'" >&2
    exit 1
fi

# --- Initialize data directory if needed -----------------------------------
if [ ! -d /var/db/mysql/mysql ]; then
    echo "Initializing MariaDB data directory..."
    /usr/local/bin/mariadb-install-db \
        --user=mysql \
        --datadir=/var/db/mysql
fi

# --- Start MariaDB and wait for it to accept connections -------------------
service mysql-server start

echo "Waiting for MariaDB to become available..."
i=0
while ! mysqladmin ping --silent >/dev/null 2>&1; do
    i=$((i + 1))
    if [ "$i" -ge 60 ]; then
        echo "ERROR: MariaDB did not come up within 60 seconds." >&2
        exit 1
    fi
    sleep 1
done

# --- Create database, user, and scoped grant --------------------------------
mysql -u root <<SQL
CREATE DATABASE IF NOT EXISTS \`${DB_NAME}\`
    DEFAULT CHARACTER SET utf8mb4
    DEFAULT COLLATE utf8mb4_general_ci;

CREATE USER IF NOT EXISTS '${DB_USER}'@'${DB_USER_HOST}'
    IDENTIFIED BY '${DB_PASSWORD}';

ALTER USER '${DB_USER}'@'${DB_USER_HOST}'
    IDENTIFIED BY '${DB_PASSWORD}';

GRANT ALL PRIVILEGES ON \`${DB_NAME}\`.* TO '${DB_USER}'@'${DB_USER_HOST}';

FLUSH PRIVILEGES;
SQL

echo "Done. '${DB_USER}'@'${DB_USER_HOST}' now has access to database '${DB_NAME}'."
echo "Remember: this grant only allows connections originating from ${DB_USER_HOST}"
echo "(the webapp jail). No '%' or 'localhost' grant was created."
