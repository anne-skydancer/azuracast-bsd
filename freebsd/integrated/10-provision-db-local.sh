#!/bin/sh
#
# freebsd/integrated/10-provision-db-local.sh
#
# One-time database provisioning for the INTEGRATED (all-in-one-jail)
# topology -- the local counterpart of freebsd/mariadb/10-provision-db.sh
# (which is for the distributed topology and scopes its grants to the
# separate webapp jail's addresses). Run INSIDE the integrated jail, after
# MariaDB is installed and freebsd/integrated/my.cnf is in place
# (freebsd/install.sh drives all of this for you).
#
# Grants are scoped to localhost/loopback ONLY -- matching my.cnf's
# loopback-only bind-address. No network-scoped or '%' grant exists in
# this topology at all.
#
# Set the password via environment (never hardcoded):
#   export AZURACAST_DB_PASSWORD='something-strong-and-unique'
#   sh freebsd/integrated/10-provision-db-local.sh
#
# Optional overrides (must then match MYSQL_DATABASE/MYSQL_USER in
# azuracast.env exactly):
#   export AZURACAST_DB_NAME='...'   # default: azuracast
#   export AZURACAST_DB_USER='...'   # default: azuracast

set -e

DB_NAME="${AZURACAST_DB_NAME:-azuracast}"
DB_USER="${AZURACAST_DB_USER:-azuracast}"
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
service mysql-server start || true

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

# --- Create database, user, and loopback-scoped grants ---------------------
# Three host forms for one logical local user: 'localhost' (unix socket /
# name resolution), '127.0.0.1' (explicit IPv4 loopback -- what
# azuracast.env's MYSQL_HOST uses in this topology), and '::1' (IPv6
# loopback, since my.cnf binds it too).
mysql -u root <<SQL
CREATE DATABASE IF NOT EXISTS \`${DB_NAME}\`
    DEFAULT CHARACTER SET utf8mb4
    DEFAULT COLLATE utf8mb4_general_ci;

CREATE USER IF NOT EXISTS '${DB_USER}'@'localhost' IDENTIFIED BY '${DB_PASSWORD}';
ALTER USER '${DB_USER}'@'localhost' IDENTIFIED BY '${DB_PASSWORD}';
GRANT ALL PRIVILEGES ON \`${DB_NAME}\`.* TO '${DB_USER}'@'localhost';

CREATE USER IF NOT EXISTS '${DB_USER}'@'127.0.0.1' IDENTIFIED BY '${DB_PASSWORD}';
ALTER USER '${DB_USER}'@'127.0.0.1' IDENTIFIED BY '${DB_PASSWORD}';
GRANT ALL PRIVILEGES ON \`${DB_NAME}\`.* TO '${DB_USER}'@'127.0.0.1';

CREATE USER IF NOT EXISTS '${DB_USER}'@'::1' IDENTIFIED BY '${DB_PASSWORD}';
ALTER USER '${DB_USER}'@'::1' IDENTIFIED BY '${DB_PASSWORD}';
GRANT ALL PRIVILEGES ON \`${DB_NAME}\`.* TO '${DB_USER}'@'::1';

FLUSH PRIVILEGES;
SQL

echo "Done. '${DB_USER}' can reach database '${DB_NAME}' from localhost,"
echo "127.0.0.1, and ::1 only -- no network-scoped grant exists."
