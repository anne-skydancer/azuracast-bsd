# webapp jail (FreeBSD, native)

This directory replaces AzuraCast's Dockerized web/app image with a
native FreeBSD jail install. This jail (named by `WEBAPP_JAIL_NAME`,
addressed by `WEBAPP_JAIL_IP`/`WEBAPP_JAIL_IP6`, and reachable at
`WEBAPP_JAIL_HOSTNAME` — all set in `freebsd/env.conf`) runs
nginx, php-fpm (PHP 8.5, matching the Docker build's `php:8.5-fpm-trixie`),
Valkey, Centrifugo, SFTPGo, cron, and supervisord (which also manages
the per-station stream-engine processes; those are generated
dynamically by the application and are out of scope here — only the
base platform is set up by this directory).

All of the addresses, paths, and hostnames mentioned in this README come
from `freebsd/env.conf` — edit that file first if your deployment uses a
different layout (see `freebsd/README.md`).

**MariaDB and Icecast are NOT set up by anything in this directory.**
They run in their own separate jails:
- MariaDB: `mariadb` jail, the value of `MARIADB_JAIL_IP` in
  `freebsd/env.conf` — see `freebsd/mariadb/`
- Icecast: your existing per-station Icecast jail(s)

This jail only needs to be able to *reach* MariaDB over TCP at
`MARIADB_JAIL_IP:MARIADB_PORT` — it does not install or manage it.

## Files here

| File | Purpose |
|---|---|
| `00-packages.sh` | Installs nginx, php85 + extensions, Valkey, ffmpeg, git, sudo, etc. via `pkg`; creates the `azuracast` system user and `/var/azuracast/*` directory layout. |
| `10-centrifugo.sh` | Builds Centrifugo v6.9.0 from source (`go install`) and installs it to `/usr/local/bin/centrifugo`. |
| `11-sftpgo.sh` | Builds SFTPGo v2.6.4 from source (`git clone` + `go build` — SFTPGo's own `go.mod` has `replace` directives that `go install pkg@version` refuses to honor, confirmed during a real install) and installs it to `/usr/local/bin/sftpgo`, along with the `templates`/`static` assets a plain `go build` doesn't bundle (also confirmed required — SFTPGo crash-loops on startup without them). |
| `20-supervisor.sh` | Installs supervisord (via `pip`) and creates its socket/log directories. |
| `supervisord.conf` | Base (non-station) supervisord config — install to `/usr/local/etc/supervisord.conf`. |
| `crontab` | Per-user crontab (for the `azuracast` user) replacing supercronic. |
| `rc.d/azuracast` | rc.d script — waits for MariaDB, runs one-time setup/migrations, then starts supervisord. Install to `/usr/local/etc/rc.d/azuracast`. |
| `nginx.conf` | Adapted nginx config (dual-stack `listen` directives) — install to `/usr/local/etc/nginx/nginx.conf`. |
| `php-fpm.d/www.conf`, `php-fpm.d/internal.conf` | php-fpm pool configs — install to `/usr/local/etc/php-fpm.d/`, **replacing** the stock `www.conf`. Required: the stock FreeBSD pool (user `www`, TCP `127.0.0.1:9000`) matches neither the unix sockets `nginx.conf`'s upstreams expect nor the `azuracast` file-ownership model, and was confirmed on a real install to make every PHP request fail silently (no sockets created, worker stderr discarded, no error log configured). |
| `centrifugo-config.toml` | Adapted Centrifugo config — install to `/var/azuracast/centrifugo/config.toml`. |
| `sftpgo.json` | Adapted SFTPGo config — install to `/var/azuracast/sftpgo/sftpgo.json`. |
| `valkey.conf` | Valkey config, scoped to IPv4 + IPv6 loopback + unix socket — install to `/usr/local/etc/valkey.conf` (or wherever the package's `valkey_config` rcvar points). |
| `configure-db.sh` | Interactive first-install script: asks whether you have an existing DB server or want the default `mariadb` jail, then writes `MYSQL_*` into `azuracast.env`. |
| `build-engine.sh` | Builds `engine/` (the Rust streaming engine) from the deployed app checkout and installs the binary to `/usr/local/bin/azuracast-engine`. Re-run after any `git pull` that touches `engine/`. |

## Setup order

1. `sh freebsd/webapp/00-packages.sh`
   Installs the base package set and creates the `azuracast` user +
   `/var/azuracast` directory tree. Every package name has been verified
   against FreshPorts (2026-07) — see the header comment for the two
   naming gotchas it corrects (`php85-pecl-redis`/`php85-pecl-uuid`, not
   the plain `php85-redis`/`php85-uuid` you'd guess, and `php85-opcache`
   no longer existing as a separate package — opcache ships bundled in
   base `php85` now).

2. `sh freebsd/webapp/10-centrifugo.sh`
   Then copy the config into place:
   ```sh
   cp freebsd/webapp/centrifugo-config.toml /var/azuracast/centrifugo/config.toml
   ```

3. `sh freebsd/webapp/11-sftpgo.sh`
   Builds from a `git clone` of the pinned tag rather than `go install`
   (see the "Files here" table above for why), and copies the
   `templates`/`static` assets out of that same checkout before cleaning
   it up. Then copy the config and generate host keys:
   ```sh
   cp freebsd/webapp/sftpgo.json /var/azuracast/sftpgo/sftpgo.json
   ssh-keygen -t rsa     -b 4096 -f /var/azuracast/storage/sftpgo/id_rsa     -q -N ""
   ssh-keygen -t ecdsa   -b 521  -f /var/azuracast/storage/sftpgo/id_ecdsa   -q -N ""
   ssh-keygen -t ed25519         -f /var/azuracast/storage/sftpgo/id_ed25519 -q -N ""
   chown -R azuracast:azuracast /var/azuracast/storage/sftpgo
   ```

4. `sh freebsd/webapp/20-supervisor.sh`
   Then copy the base supervisord config:
   ```sh
   cp freebsd/webapp/supervisord.conf /usr/local/etc/supervisord.conf
   ```

5. Copy the nginx config and the php-fpm pool configs:
   ```sh
   cp freebsd/webapp/nginx.conf /usr/local/etc/nginx/nginx.conf
   cp freebsd/webapp/php-fpm.d/www.conf /usr/local/etc/php-fpm.d/www.conf
   cp freebsd/webapp/php-fpm.d/internal.conf /usr/local/etc/php-fpm.d/internal.conf
   ```
   The pool configs are NOT optional and the `www.conf` copy deliberately
   **replaces** the package's stock pool — `nginx.conf`'s upstreams point
   at the two unix sockets these pools create
   (`/var/run/php-fpm-www.sock`, `/var/run/php-fpm-internal.sock`);
   with the stock pool left in place those sockets never exist and every
   PHP request fails silently (see `php-fpm.d/www.conf`'s header comment
   for the full confirmed-on-real-hardware failure mode).
   Also apply the M3U8 MIME-type fix Docker's `nginx.sh` applies
   (`application/vnd.apple.mpegurl` -> `application/x-mpegurl`) to
   `/usr/local/etc/nginx/mime.types` by hand — this wasn't scripted
   here since it's a one-line edit to a package-owned file.

6. Install and enable the rc.d script:
   ```sh
   cp freebsd/webapp/rc.d/azuracast /usr/local/etc/rc.d/azuracast
   chmod +x /usr/local/etc/rc.d/azuracast
   sysrc azuracast_enable=YES
   ```
   Set `azuracast_path` (where the PHP application is actually
   deployed — deploying the app itself is out of scope for this
   change) via `/etc/rc.conf.d/azuracast`, e.g.:
   ```sh
   echo 'azuracast_path="/usr/local/www/azuracast"' >> /etc/rc.conf.d/azuracast
   ```

7. Install the crontab for the `azuracast` user (update `AZURACAST_PATH`
   inside the file first, same caveat as above):
   ```sh
   crontab -u azuracast freebsd/webapp/crontab
   ```

8. Configure the database connection (interactive, first-install only):
   ```sh
   export AZURACAST_PATH=/usr/local/www/azuracast   # wherever the app is deployed
   sh freebsd/webapp/configure-db.sh
   ```
   Asks whether you already have a MariaDB/MySQL-compatible server set
   up. If yes, it prompts for that server's host/port/database/user/
   password directly. If no, it defaults to this project's documented
   topology (the dedicated `mariadb` jail at `MARIADB_JAIL_IP:MARIADB_PORT`
   — see `freebsd/mariadb/README.md`) and only prompts for the database
   name/user/password, which it then reminds you to provision on that jail
   with matching values. Either way it writes `MYSQL_*` into
   `<AZURACAST_PATH>/azuracast.env` (mode 600, `azuracast`-owned),
   which is the exact file `backend/src/Environment.php` reads —
   confirm that file is `.gitignore`d wherever the app is deployed from,
   since it now holds a real password.

9. Build and install the streaming engine binary:
   ```sh
   export AZURACAST_PATH=/usr/local/www/azuracast   # same as step 8
   sh freebsd/webapp/build-engine.sh
   ```
   Installs a Rust toolchain (`pkg install rust`) if `cargo` isn't already
   present, builds `engine/` in release mode, and installs the resulting
   binary to `/usr/local/bin/azuracast-engine` — the exact path
   `StreamEngine::getBinary()` expects. Without this step, stations using
   the `stream_engine` backend (the only backend this fork supports) will
   fail to start, since supervisord will try to exec a binary that doesn't
   exist yet. Re-run this step after any `git pull` that touches `engine/`.

10. Start everything: `service azuracast start`
    (this waits for MariaDB, runs `azuracast:setup --init`, then starts
    supervisord, which brings up nginx/php-fpm/centrifugo/sftpgo).

### Valkey

Valkey/Redis is installed by `00-packages.sh` (`pkg install ... valkey`)
but is **not** managed by supervisord — it runs as its own native
FreeBSD rc.d service, independent of the `azuracast` service above.
`00-packages.sh` does not enable or configure it any further than
installing the package, so before step 8 you still need to:

```sh
sysrc valkey_enable=YES
cp freebsd/webapp/valkey.conf /usr/local/etc/valkey.conf
```

(confirm `/usr/local/etc/valkey.conf` against the installed package's
default `valkey_config` rcvar — adjust the path if it differs). Note
`valkey.conf` here deliberately binds to `127.0.0.1` + `::1` (IPv4 and
IPv6 loopback) + a unix socket rather than `0.0.0.0` as the Docker
source does — nothing outside this jail needs to reach Valkey, so it's
scoped down the same way the `mariadb` jail's `bind-address` is. The
`::1` addition is for dual-stack completeness only; Valkey is
deliberately **not** bound to `WEBAPP_JAIL_IP6` (this jail's routable
IPv6 address) — doing so would defeat the loopback-only posture this
was scoped down to in the first place.

Then:
```sh
service valkey start
```

## `.env` / `azuracast.env` values this jail needs

The `MYSQL_*` block is written for you by `configure-db.sh` (step 8
above) rather than hand-edited — see that step for the interactive
"do you already have a DB server?" flow. The rest of the values this
jail needs, either set alongside it or already implied by the default
topology:

```
APPLICATION_ENV=production

ENABLE_REDIS=true
REDIS_HOST=localhost
REDIS_PORT=6379
REDIS_DB=0
```

`REDIS_HOST` stays `localhost` because Valkey runs inside this same
jail, not a separate one. If you chose the default topology in
`configure-db.sh` (no existing server), `MYSQL_HOST` ends up as the
separate `mariadb` jail (the value of `MARIADB_JAIL_IP` in
`freebsd/env.conf`) — see `freebsd/mariadb/README.md` for how that
user/grant was provisioned (scoped to `'azuracast'@'WEBAPP_JAIL_IP'`,
i.e. this jail's own IP, from `env.conf`).

These are the same env var names AzuraCast's `backend/src/Environment.php`
already reads — no application code changes are required, only values.
Where exactly the AzuraCast application itself gets deployed to (so
`azuracast.env` has somewhere to live) is out of scope for this change.

## Dual-stack (IPv4 + IPv6)

This jail is dual-stack (`WEBAPP_JAIL_IP` / `WEBAPP_JAIL_IP6` in
`freebsd/env.conf`). What was audited and changed at the application
layer:

- **nginx** (`nginx.conf`): every `listen` directive now has a matching
  IPv6 counterpart — `listen 80;` / `listen [::]:80;`, `listen 443
  default_server http2 ssl;` / `listen [::]:443 default_server http2
  ssl;`, and the internal `127.0.0.1:6010` handler also listens on
  `[::1]:6010`. Previously nginx was IPv4-only despite the jail having a
  routable IPv6 address.
- **Valkey** (`valkey.conf`): now binds `127.0.0.1 ::1` (was
  `127.0.0.1` only) — loopback-only by design either way (see the
  Valkey section above); the IPv6 loopback addition is for
  completeness, not for exposing Valkey beyond this jail.
- **Centrifugo** (`centrifugo-config.toml`): no change needed.
  `[http_server]` sets only `port`/`internal_port`, no explicit
  `address`, and Centrifugo's documented behavior for an unset address
  is to listen on the wildcard for both address families — confirmed by
  reading Centrifugo's own docs rather than assumed.
- **SFTPGo** (`sftpgo.json`): no change needed. `sftpd.bindings[0].address`
  is `""`, which is SFTPGo's documented value for "listen on all
  interfaces" — dual-stack already, per SFTPGo's own binding docs.
  Documented in a `_comment_bindings` key added next to the binding.
- **MariaDB reachability from this jail**: see
  `freebsd/mariadb/README.md`'s new dual-stack section — MariaDB now
  binds both `MARIADB_JAIL_IP` and `MARIADB_JAIL_IP6`, with matching
  IPv4/IPv6-scoped grants for `'azuracast'@'WEBAPP_JAIL_IP'` /
  `'azuracast'@'WEBAPP_JAIL_IP6'`.
- **`configure-db.sh`**: the "no existing server" branch still defaults
  `_db_host` to `MARIADB_JAIL_IP` (IPv4) — left as-is deliberately, since
  IPv4 has no practical disadvantage over IPv6 on this internal
  single-host jail network, and adding an interactive IPv6 option would
  be UI complexity with no real benefit. A comment in the script notes
  `MARIADB_JAIL_IP6` is available and can be substituted by hand if you
  prefer it.

## Notes / things worth double-checking on the actual box

- **PHP version**: matches the Docker build's `FROM php:8.5-fpm-trixie`
  — `00-packages.sh` installs `php85*` packages. Verified against
  FreshPorts (2026-07): `lang/php85` is a mature 8.5.8_1, not an early
  snapshot.
- **`security/openssl` version transition**: FreeBSD's pkg-message for
  `security/openssl` (currently 3.0.21) warns of a base-port move from
  OpenSSL 3.0 → 3.5 around 2026Q3 due to 3.0's EOL — worth checking
  which side of that transition you land on when you actually run
  `00-packages.sh`, in case pinning is needed.
- **`audiowaveform`** (waveform preview generation) has no FreeBSD port
  and is not installed anywhere in this directory — see the note in
  `00-packages.sh` for manual build instructions if it's needed.
- **MaxMind DB-IP GeoIP database** (`util/docker/web/setup/dbip.sh`) is
  a data download, not a package; a commented-out monthly cron entry
  for it is included in `crontab` but disabled by default.
- **supervisord install method**: `20-supervisor.sh` uses `pip install
  supervisor` to mirror the Docker build exactly; `sysutils/py-supervisor`
  (e.g. `py311-supervisor`) is a `pkg`-only alternative noted in that
  script's comments but not used by default.
- **Centrifugo/SFTPGo versions**: pinned to v6.9.0 / v2.6.4 respectively,
  matching `util/docker/web/setup/centrifugo.sh` and `sftpgo.sh` exactly.
  Both are built from source since neither publishes FreeBSD binaries —
  Centrifugo via `go install` (its `go.mod` has no `replace` directives),
  SFTPGo via `git clone` + `go build` (its `go.mod` does have `replace`
  directives, which `go install pkg@version` refuses to honor — confirmed
  during a real install; see `11-sftpgo.sh`'s header comment).
- This directory's scripts have now been exercised against a real
  FreeBSD box once (2026-07) — the fixes above (extra PHP extensions,
  php-fpm/nginx/supervisord socket and PID ownership, a self-signed cert
  placeholder, the SFTPGo build method) came from that run. Treat
  anything not called out above as still a well-researched first draft,
  not independently re-verified since.
