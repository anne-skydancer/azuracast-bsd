# mariadb jail (FreeBSD, native)

This directory replaces AzuraCast's Dockerized MariaDB with a native
FreeBSD jail install. This jail (named by `MARIADB_JAIL_NAME`, addressed
by `MARIADB_JAIL_IP`/`MARIADB_JAIL_IP6`, and reachable at
`MARIADB_JAIL_HOSTNAME` — all set in `freebsd/env.conf`) runs **only
MariaDB** — no PHP, nginx, Redis/Valkey, or any other AzuraCast
component. The application itself (PHP/nginx) runs separately in the
`webapp` jail (the value of `WEBAPP_JAIL_IP` in `freebsd/env.conf`),
which connects to this jail on the port set by `MARIADB_PORT`.
Restricting network access to that port to only the `webapp` jail (via
`pf` or otherwise) is left entirely to your own firewall policy — out
of scope for this project.

All of the addresses, paths, and hostnames mentioned in this README come
from `freebsd/env.conf` — edit that file first if your deployment uses a
different layout, then re-run `freebsd/generate-jail-conf.sh` (see
`freebsd/README.md`) and `00-install.sh` to pick up the change everywhere.

## Files here

| File | Purpose |
|---|---|
| `00-install.sh` | Installs `mariadb118-server` via `pkg`, enables it at boot, and renders `my.cnf.tmpl` from `freebsd/env.conf`. |
| `my.cnf.tmpl` | Config fragment template — rendered by `00-install.sh` directly to `/usr/local/etc/mysql/conf.d/azuracast.cnf`. |
| `10-provision-db.sh` | First-run only: initializes the data dir, starts MariaDB, creates the `azuracast` database/user (IPv4 + IPv6 grants) with network-scoped grants. |

## Dual-stack (IPv4 + IPv6)

This jail is dual-stack (`MARIADB_JAIL_IP` / `MARIADB_JAIL_IP6` in
`freebsd/env.conf`), and both application-layer pieces here now actually
use both addresses, not just the network layer:

- `my.cnf.tmpl`'s `bind-address` is a comma-separated list —
  `@@MARIADB_JAIL_IP@@,@@MARIADB_JAIL_IP6@@` — which MariaDB has
  supported since 10.4.6 (well below the 11.8 branch this jail installs).
  `00-install.sh`'s `sed` invocation substitutes both tokens from
  `env.conf`. This means `webapp` can reach MariaDB over either its IPv4
  or IPv6 address.
- Because a bind address alone doesn't grant access, `10-provision-db.sh`
  now creates **two** users for the same logical account —
  `'azuracast'@'WEBAPP_JAIL_IP'` and `'azuracast'@'WEBAPP_JAIL_IP6'` —
  both sharing the same `AZURACAST_DB_PASSWORD`, so a webapp connection
  authenticates correctly regardless of which address family it connects
  over.

## Setup order

1. `sh freebsd/mariadb/00-install.sh`
   Installs `mariadb118-server`, runs `sysrc mysql_enable=YES`, and
   renders `my.cnf.tmpl` to `/usr/local/etc/mysql/conf.d/azuracast.cnf`
   using the values in `freebsd/env.conf`.

2. (Handled by step 1 above — `my.cnf.tmpl` is rendered and installed to
   `/usr/local/etc/mysql/conf.d/azuracast.cnf` automatically.)

3. Set the database password (do this before step 4):

   ```sh
   export AZURACAST_DB_PASSWORD='your-strong-password-here'
   ```

   `10-provision-db.sh` refuses to run if this is left unset (it falls back
   to a placeholder value, `CHANGE_ME_SET_VIA_ENV`, and exits with an error
   rather than silently provisioning a weak/default credential).

   Optionally also set `AZURACAST_DB_NAME` and/or `AZURACAST_DB_USER` if
   you want a database name/user other than the default `azuracast` for
   either — neither is hardcoded in the app, they're just config values
   that must match `MYSQL_DATABASE`/`MYSQL_USER` in the `webapp` jail's
   `.env` exactly:

   ```sh
   export AZURACAST_DB_NAME='whatever-you-want'
   export AZURACAST_DB_USER='whatever-you-want'
   ```

4. `sh freebsd/mariadb/10-provision-db.sh`
   Initializes `/var/db/mysql` if needed, starts `mysql-server`, and creates:
   - database `azuracast` (utf8mb4 / utf8mb4_general_ci)
   - user `'azuracast'@'WEBAPP_JAIL_IP'` **and**
     `'azuracast'@'WEBAPP_JAIL_IP6'` (scoped to the webapp jail's IPv4
     and IPv6 addresses, both values from `freebsd/env.conf` — i.e. the
     webapp jail's addresses only — deliberately **not**
     `'azuracast'@'%'` and **not** `'azuracast'@'localhost'`, since the
     app connects over the network from a different jail). Both users
     share the same password (`AZURACAST_DB_PASSWORD`) — this is one
     logical account reachable from either address family, not two
     separate accounts.
   - a grant of all privileges on `azuracast`.* to each of those users

   Note the MariaDB user grant above is scoped to the `webapp` jail's IP,
   but that's an authentication-layer restriction, not a network-layer
   one — this project does not set up a firewall rule restricting which
   hosts can reach `MARIADB_JAIL_IP:MARIADB_PORT` at the TCP level. If
   you want that, it's on you to configure via `pf`/`ipfw`/whatever you
   use, matching your own firewall policy.

## `.env` values for the `webapp` jail

Point the AzuraCast application (running in the `webapp` jail — the value
of `WEBAPP_JAIL_IP` in `freebsd/env.conf`) at this jail with:

```
MYSQL_HOST=<value of MARIADB_JAIL_IP in freebsd/env.conf>
MYSQL_PORT=<value of MARIADB_PORT in freebsd/env.conf>
MYSQL_DATABASE=<value of AZURACAST_DB_NAME_DEFAULT in freebsd/env.conf, or AZURACAST_DB_NAME if you overrode it>
MYSQL_USER=<value of AZURACAST_DB_USER_DEFAULT in freebsd/env.conf, or AZURACAST_DB_USER if you overrode it>
MYSQL_PASSWORD=<the password you set via AZURACAST_DB_PASSWORD in step 3>
```

(all four variable names above are defined in `freebsd/env.conf`)

These are the same env var names AzuraCast's `backend/src/Environment.php`
(`getDatabaseSettings()`) already reads, so no application code changes are
required — only the values.

## Notes / things worth double-checking on the actual box

- MariaDB version: AzuraCast's Docker build pins the server to **11.8.3**
  (see `util/docker/mariadb/setup/mariadb.sh`, `--mariadb-server-version`).
  The closest FreeBSD package is `mariadb118-server` (11.8 branch, currently
  packaged at 11.8.8 per FreshPorts) — same LTS branch, newer patch level.
  This was not tested on an actual FreeBSD box as part of this change.
- The rc.d service name/rcvar for `mariadb118-server` is `mysql-server` /
  `mysql_enable` (legacy naming retained for compatibility), not
  `mariadb-server` / `mariadb_enable` — verified against FreshPorts, but
  worth a sanity check with `service mysql-server status` after install in
  case a future ports revision renames it.
- Default datadir (`/var/db/mysql`) and install helper
  (`mariadb-install-db`) are standard FreeBSD conventions for MySQL-family
  ports; confirm the exact binary name/path shipped by `mariadb118-server`
  on the target FreeBSD release if `10-provision-db.sh` fails at that step.
