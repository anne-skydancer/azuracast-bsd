# Installing azuracast-bsd

This fork replaces AzuraCast's Docker Compose stack with native FreeBSD jails, and replaces its
Liquidsoap (OCaml) streaming engine with a Rust engine (`engine/`). There is no Docker image,
`docker-compose.yml`, or one-click installer for this fork — installation means bringing up a
handful of FreeBSD jails by hand and deploying the application into them. This document is the
linear, start-to-finish path; the `freebsd/*/README.md` files it links to are the detailed
per-component references.

**Status:** this fork's `freebsd/` scripts and the `engine/` Rust build have been written against
FreeBSD's documented package/port behavior and this project's own reference deployment, but have
not yet been exercised end-to-end against a real FreeBSD box as of this writing. Package names,
paths, and versions should be treated as a well-researched first draft — if something doesn't
match what you see on your box, that's expected territory, not a sign you've misread these
instructions.

## Prerequisites

- A FreeBSD host with jail support (plain `/etc/jail.conf`, VNET-per-jail — no bastille/iocage
  assumed by any script here) and enough disk/network setup to bridge jail interfaces to your
  network (`vm-public` bridge convention, see `freebsd/env.conf`).
- An existing Icecast frontend jail (or willingness to set one up with stock `audio/icecast` from
  ports) for each station — this fork's `mariadb`/`webapp` jails do not include Icecast; see
  `freebsd/icecast/README.md` if you want AzuraCast to manage a remote Icecast jail's process
  lifecycle via supervisord.
- A GitHub account with access to your fork of this repository (public: nothing extra needed;
  private: an SSH deploy key or personal access token — see step 5 below).

## 1. Review and edit `freebsd/env.conf`

Every IP, hostname, path, and epair interface number used across the whole `freebsd/` tree comes
from this one file. **The values shipped in it are not real** — they're placeholders drawn
deliberately from IETF's documentation-reserved ranges (`192.0.2.0/24` / `2001:db8::/32`, RFC
5737/3849) and the reserved `example.com` domain (RFC 2606), specifically so this repository never
has to contain anyone's actual network topology. They are guaranteed by IETF to never be assigned
to a real host and **will not work on a real network as shipped.** Open the file and set every
variable in it — `MARIADB_JAIL_NAME`, `MARIADB_JAIL_IP`/`IP6`, `WEBAPP_JAIL_NAME`,
`WEBAPP_JAIL_IP`/`IP6`, the netmask/prefix-length variables, the epair numbers, the default-route
addresses, the hostnames — to your own host's jail registry and network layout before doing
anything else. Nothing below this step should be read as "your setup will look like this";
substitute your own `env.conf` values throughout.

## 2. Generate and install the jail stanzas

```sh
sh freebsd/generate-jail-conf.sh
```

Renders `freebsd/jail.conf.d/mariadb.conf` and `webapp.conf` from your edited `env.conf`. Merge
both into your host's `/etc/jail.conf` (or your own per-jail include mechanism), then start both
jails using whatever names you set for `MARIADB_JAIL_NAME`/`WEBAPP_JAIL_NAME` in `env.conf` (the
examples below use the shipped defaults, `mariadb`/`webapp` — substitute your own if you changed
them):

```sh
service jail start mariadb
service jail start webapp
```

## 3. Provision the `mariadb` jail

Follow **[`freebsd/mariadb/README.md`](freebsd/mariadb/README.md)** in full — package install,
`bind-address` configuration, and creating the `azuracast` database/user scoped to the `webapp`
jail's IP. Do this before touching `webapp`: its rc.d script waits on MariaDB at boot.

## 4. Provision the `webapp` jail's base platform

Follow **[`freebsd/webapp/README.md`](freebsd/webapp/README.md)**'s "Setup order" section
(steps 1–8 there): package install (`00-packages.sh`), Centrifugo/SFTPGo build-from-source
(`10-centrifugo.sh`/`11-sftpgo.sh`), supervisord install (`20-supervisor.sh`), nginx/rc.d/crontab
installation, and Valkey setup. This provisions the OS-level platform (nginx, php85, node22/npm,
git, composer, ffmpeg, Valkey, Centrifugo, SFTPGo, supervisord) but does **not** deploy the
application itself — that's the next step.

## 5. Get onto the `webapp` jail and clone this repository

Everything from here runs **inside** the `webapp` jail (whatever you named it in `env.conf`), not
on the bare host and not on whatever machine you're reading this from. From the host:

```sh
jexec <your-webapp-jail-name> sh
```

(or SSH directly to the jail's own address if you've set up `sshd` inside it — either way, you
need a shell *inside* the jail for the remaining steps.)

Then clone your fork of this project to **exactly `/var/azuracast/www`** — this path is NOT a
free choice. The application derives every runtime path (its temp dir, storage, uploads, station
configs, ACME certs) from *the parent directory of wherever it's deployed* (see
`backend/src/Environment.php` — e.g. temp is `<parent>/www_tmp`), and `freebsd/webapp/nginx.conf`'s
document root is hardcoded to `/var/azuracast/www/web`. Deploying at `/var/azuracast/www` makes all
of those resolve to the `/var/azuracast/*` tree that `00-packages.sh` already created and owned
correctly; deploying anywhere else makes the app compute paths into an unprepared, likely
root-owned tree, and fails at runtime in confusing ways (confirmed the hard way on a real
install). `00-packages.sh` creates `/var/azuracast/www` as an empty directory, so clone into it:

```sh
git clone <your-fork-url> /var/azuracast/www
chown -R azuracast:azuracast /var/azuracast/www
cd /var/azuracast/www
```

(The `chown` matters: if you clone as root, the app — which runs as `azuracast` — can't write its
own caches or have files deployed over it later.)

If your repository is private, either:
- use an SSH remote (`git clone git@github.com:<you>/<your-fork>.git ...`) with a deploy key added
  to the jail's `azuracast` user (or root, for the initial clone) and to the repo's GitHub deploy
  keys, or
- use an HTTPS URL with a GitHub personal access token embedded
  (`https://<token>@github.com/...`) or supplied when prompted.

## 6. Install PHP and JS dependencies, build frontend assets

```sh
composer install --no-dev --no-ansi --no-interaction
composer dump-autoload --optimize --classmap-authoritative
npm ci --include=dev
npm run build
```

## 7. Configure the database connection

```sh
export AZURACAST_PATH=/var/azuracast/www
sh freebsd/webapp/configure-db.sh
```

Interactive, first-install only. Asks whether you already have a MariaDB/MySQL-compatible server
(if you followed step 3 and are using the default topology, answer accordingly and it'll default
to the `mariadb` jail's address) and writes the resulting `MYSQL_*` values into
`azuracast.env` (mode 600, `azuracast`-owned — confirm this file never gets committed anywhere,
since it now holds a real password).

## 8. Build and install the streaming engine

```sh
sh freebsd/webapp/build-engine.sh
```

Installs a Rust toolchain (`pkg install rust`) if one isn't already present, builds `engine/` in
release mode, and installs the resulting binary to `/usr/local/bin/azuracast-engine` — the exact
path the PHP application expects. **Without this step, no station can start** — supervisord will
try to exec a binary that doesn't exist yet. Re-run this specific step after any future `git pull`
that touches `engine/`.

## 9. Start everything

```sh
service azuracast start
```

Waits for MariaDB to be reachable, runs `azuracast:setup --init` (first-run database
migration/initialization), then starts supervisord, which brings up nginx, php-fpm, Centrifugo,
and SFTPGo.

## 10. Set up each station's Icecast frontend

Per station, you need an Icecast jail already running `audio/icecast` (or your own build) —
this fork does not install or manage Icecast itself. To let AzuraCast's PHP side manage that
jail's Icecast process lifecycle remotely via supervisord, follow
**[`freebsd/icecast/README.md`](freebsd/icecast/README.md)**. If you'd rather manage Icecast
entirely by hand outside AzuraCast, that's also fine — just don't apply that scaffold, and
configure the station's frontend connection details to point at the jail directly.

## Updating later

```sh
cd /var/azuracast/www
git pull
composer install --no-dev --no-ansi --no-interaction
composer dump-autoload --optimize --classmap-authoritative
npm ci --include=dev && npm run build
sh freebsd/webapp/build-engine.sh   # only strictly required if engine/ changed, but harmless otherwise
service azuracast restart
```

## If something doesn't work

This is a new fork's install path, not a battle-tested one yet — see the "Status" note above.
Each `freebsd/*/README.md` has its own "things worth double-checking" section calling out specific
unconfirmed points (exact package names, a couple of inferred PHP 8.5 extension names,
`audiowaveform` having no FreeBSD port). If you hit something not covered there, that's useful
information to bring back, not necessarily a sign of a mistake in these instructions.
