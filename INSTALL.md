# Installing azuracast-bsd

This fork replaces AzuraCast's Docker Compose stack with native FreeBSD jails, and replaces its
Liquidsoap (OCaml) streaming engine with a Rust engine (`engine/`). There is no Docker image,
`docker-compose.yml`, or one-click installer for this fork — installation means bringing up a
handful of FreeBSD jails by hand and deploying the application into them. This document is the
linear, start-to-finish path; the `freebsd/*/README.md` files it links to are the detailed
per-component references.

**Status:** this path has been exercised **end-to-end on a real FreeBSD 15.1 host** (2026-07): a
station built this way is live on air — engine → per-station Icecast jail → nginx `/listen` proxy,
with HTTPS via a real Let's Encrypt certificate, in-stream song metadata, and automatic
wedge-recovery. The hard-won fixes from that install (php-fpm pool configs, the rc.d
`${name}_user` collision, the mandatory deploy path, Icecast 2.5-beta hardening, source-limit
headroom, the combined TLS PEM) are all committed — following this document as written lands on
the battle-tested configuration. Where a specific step was confirmed the hard way, the linked
README says so.

## Choosing a topology (and the guided installer)

There are two supported layouts, and a guided installer that drives either one:

```sh
sh freebsd/install.sh --mode distributed   # or --mode integrated, or no flag to be asked
```

- **Distributed** (the reference deployment, and what the rest of this document describes
  step-by-step): a `mariadb` jail, a `webapp` jail, and one Icecast jail per station. Best
  isolation and blast-radius control; per-station Icecast jails remain a manual template
  (`freebsd/icecast/README.md`) since they're typically pre-existing jails an installer shouldn't
  reshape.
- **Integrated**: everything — MariaDB (loopback-only), the web stack, the streaming engine, and
  Icecast — inside **one** jail. No cross-jail nullfs mounts, no remote supervisord, station
  frontends default to co-located `127.0.0.1`. Simpler; less isolation. *(For future reference:
  the planned Linux/Docker sister project will offer this topology only — one container, no
  distributed refinement.)*

The installer runs as root **on the host**, gates every state change behind a confirmation
prompt, can fetch/extract `base.txz` if a jail's rootfs doesn't exist yet, and executes the same
per-component scripts this document walks through by hand. Either way, **edit `freebsd/env.conf`
first** (step 1 below) — the installer refuses to run against the shipped placeholder addresses.
The manual steps below remain the authoritative reference for what the installer does and for
picking up mid-way when something environment-specific needs a human.

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

## 1. Create and edit `freebsd/env.conf`

```sh
cp freebsd/env.conf.example freebsd/env.conf
```

Every IP, hostname, path, and epair interface number used across the whole `freebsd/` tree comes
from this one file. Your copy is `.gitignore`d — deliberately, so your real network topology is
never committed and never collides with upstream changes to the template. **The values shipped in
the template are not real** — they're placeholders drawn
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
git config --global --add safe.directory /var/azuracast/www
cd /var/azuracast/www
```

(The `chown` matters: if you clone as root, the app — which runs as `azuracast` — can't write its
own caches or have files deployed over it later. The `safe.directory` line matters for the same
reason from the other side: once the checkout is `azuracast`-owned, every future `git pull` you
run as root fails with "detected dubious ownership" until root's git is told the directory is
trusted — one-time setup, confirmed on a real install.)

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
git log -1 --oneline   # VERIFY the hash is the commit you expect before rebuilding anything
composer install --no-dev --no-ansi --no-interaction
composer dump-autoload --optimize --classmap-authoritative
npm ci --include=dev && npm run build
sh freebsd/webapp/build-engine.sh   # only strictly required if engine/ changed, but harmless otherwise
service azuracast restart
```

Notes from the reference deployment's own update loop:

- **Verify, don't assume.** The two silent ways an "update" deploys nothing: the `git pull` failed
  (dubious-ownership when run as root without the `safe.directory` config from step 5, or a
  network error scrolled past) and the engine build was skipped (a successful build that compiled
  new code prints a `Compiling azuracast-engine` line; `Finished` alone means the binary was
  already current). Check `git log -1` and watch for `Compiling` — both bit the reference install.
- For an engine-only change, a lighter cycle than `service azuracast restart` is to restart just
  the station backends: `supervisorctl restart station_<id>_backend` per station (or
  `su -m azuracast -c 'php /var/azuracast/www/backend/bin/console azuracast:radio:restart'`).
- Any restart drops each station's source connection for a few seconds. Listeners ride it out on
  the station's fallback file **if you installed one** (see `freebsd/icecast/README.md`, step 10)
  — browser players never reconnect on their own otherwise.

## Operational gotchas (all confirmed on the reference install)

- **After cycling an Icecast jail** (`service jail restart <station-jail>`), the station frontend
  may come up `STOPPED` — `azuracast:radio:restart` races the jail's freshly-booting supervisord
  and its start can silently miss. Fix: `jexec <station-jail> supervisorctl start
  station_<id>_frontend`.
- **"No sound" after any restart usually isn't the stream.** Players (VLC, browser audio) hold a
  dead socket across an Icecast restart and sit in silence on top of a healthy mount — reconnect
  the player *first*, then diagnose.
- **Saving station configuration restarts the frontend** (this fork deliberately restarts instead
  of SIGHUP-reloading Icecast — the 2.5 beta degrades after a HUP). Treat station config edits as
  brief announced maintenance: every listener gets bumped to the fallback and back.
- **Duplicate `mount +=` lines in a jail stanza** produce nullfs's cryptic "Resource deadlock
  avoided" at jail start. Check for accidental duplicates before suspecting anything deeper.
- **Media over NFS**: mount it via the *jail's* fstab mechanism (`mount += ...` in the stanza),
  not rc.local, and keep the station's fallback file on jail-local disk — its whole job is to
  play when remote things fail.

## If something doesn't work

Each `freebsd/*/README.md` has a "things worth double-checking" section for its component, with
the confirmed-on-real-hardware failure modes called out inline where they were found
(php-fpm pools, rc.subr variable collision, proxy_params, Icecast beta wedges). If you hit
something not covered there, that's useful information to bring back, not necessarily a sign of a
mistake in these instructions.
