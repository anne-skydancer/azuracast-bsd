# AzuraCast on FreeBSD jails (no Docker)

This directory replaces AzuraCast's Docker Compose stack with three
native FreeBSD jails on the same host as your existing per-station
Icecast jails, sharing their `jail.conf` schema (VNET + `epairN`,
static internal IPv4/IPv6, default routes via `DEFAULT_ROUTE_V4` in
`env.conf`).

## Before you do anything else

Every IP, hostname, path, and epair interface used across this whole
`freebsd/` tree lives in one place: [`freebsd/env.conf`](env.conf).
**The values shipped in that file are not real** — they're IETF
documentation-reserved placeholders (`192.0.2.0/24` / `2001:db8::/32` per
RFC 5737/3849, plus the reserved `example.com` domain per RFC 2606), used
specifically so this repository never has to contain anyone's actual
network topology. They will not work on a real network as shipped. Review
and edit that file to match your own jail/network layout first, then run:

```sh
sh freebsd/generate-jail-conf.sh
```

to render `jail.conf.d/mariadb.conf` and `jail.conf.d/webapp.conf` from
their `.tmpl` templates using your `env.conf` values. Every other script
under `mariadb/` and `webapp/` also sources `env.conf` directly (or, for
the one rc.d script that must run standalone at jail boot, documents the
`sysrc` variable you need to set instead).

| Jail | IP | Purpose | Setup |
|---|---|---|---|
| `mariadb` (name configurable via `MARIADB_JAIL_NAME`) | value of `MARIADB_JAIL_IP`/`MARIADB_JAIL_IP6` in `env.conf` | Database only | [`freebsd/mariadb/`](mariadb/README.md) |
| `webapp` (name configurable via `WEBAPP_JAIL_NAME`) | value of `WEBAPP_JAIL_IP`/`WEBAPP_JAIL_IP6` in `env.conf` | nginx, php-fpm, Valkey, Centrifugo, SFTPGo, cron, supervisord | [`freebsd/webapp/`](webapp/README.md) |
| one jail per station | (existing, per-station, whatever you've named them) | Audio frontend (Icecast) | Icecast itself already set up outside this change per jail — see Phase 1's Risk #1 in the project plan re: the custom Icecast-KH fork, still unaddressed. [`freebsd/icecast/`](icecast/README.md) is a *template* (not a fixed single jail like the two rows above) that makes an already-running Icecast jail's supervisord remotely manageable by AzuraCast's PHP side — it does not install or build Icecast itself. |

`jail.conf.d/mariadb.conf` and `jail.conf.d/webapp.conf` hold the actual
jail stanzas (generated from the `.tmpl` files by
`generate-jail-conf.sh`, as above) — merge these into your host's
`/etc/jail.conf` (or your per-jail include mechanism) before doing
anything else in either subdirectory.

## Bring-up order

1. Review/edit `freebsd/env.conf`, then run `sh freebsd/generate-jail-conf.sh`
   to render the two `jail.conf.d/*.conf` stanzas. Add both to
   `/etc/jail.conf`, then start both jails.
2. Provision `mariadb` first (see `mariadb/README.md`) — `webapp`'s
   rc.d script waits on it at boot, and its database user grant is
   scoped specifically to `webapp`'s IP (the value of `WEBAPP_JAIL_IP`
   in `env.conf`), so get the DB up before starting `webapp`.
3. Provision `webapp` (see `webapp/README.md`).
4. Deploy the AzuraCast PHP application itself into `webapp` (git
   clone/composer install/frontend build) — this isn't covered by
   either subdirectory, which only set up the surrounding OS-level
   platform, not the app deployment step. Then build and install the
   Rust streaming engine binary via `freebsd/webapp/build-engine.sh`
   (see `freebsd/webapp/README.md`'s setup order, step 9) — without
   this, no station can actually start.
5. Confirm `webapp` can reach `mariadb` on the value of `MARIADB_PORT` in
   `env.conf`. Restricting network
   access beyond that (via `pf` or otherwise) is your own firewall
   policy to set up if you want it — out of scope for this project,
   which relies on jail isolation as the actual security boundary.

## What's still a stub at the end of this phase

Per the project plan, each station's actual backend (currently
Liquidsoap) is intentionally left as a placeholder in this phase — the
goal here is proving the *platform* (jails, networking, MariaDB, the
web stack) is solid before the streaming-engine replacement work
starts. See `engine/SPEC.md` for the behavioral spec that replacement
engine must satisfy, and the project plan file for the phase-by-phase
sequencing.

## Dual-stack (IPv4 + IPv6) audit

Both jails are dual-stack at the network level (`jail.conf.d/*.tmpl`
already configure each jail's IPv4 *and* IPv6 address). This audit
checked whether the application-layer configs under `mariadb/` and
`webapp/` actually use both addresses, and fixed the ones that didn't:

- **MariaDB** now binds both `MARIADB_JAIL_IP` and `MARIADB_JAIL_IP6`
  (comma-separated `bind-address`, supported since MariaDB 10.4.6), with
  matching IPv4/IPv6-scoped grants for the `webapp` DB user. See
  `mariadb/README.md`'s dual-stack section.
- **nginx** now has a `listen [::]:PORT ...;` counterpart for every
  IPv4 `listen` directive. See `webapp/README.md`'s dual-stack section.
- **Valkey** now also binds IPv6 loopback (`::1`) alongside `127.0.0.1`
  — still loopback-only by design, not exposed on `WEBAPP_JAIL_IP6`.
- **Centrifugo** and **SFTPGo** were already dual-stack by default
  (unset/empty bind address = listen on all interfaces, both families)
  — confirmed by reading each project's own docs, no changes needed.
- `configure-db.sh`'s default DB host prompt intentionally still
  defaults to IPv4 (`MARIADB_JAIL_IP`) — see `webapp/README.md` for why.

## Known open items

- Neither jail's scripts have been run against a real FreeBSD box as
  part of this change — treat package names, paths, and versions as a
  well-researched first draft. Each subdirectory's README has a "things
  worth double-checking" section calling out specific unconfirmed
  points (a few inferred PHP 8.5 extension package names,
  `audiowaveform` having no FreeBSD port, MariaDB's exact rc.d service
  name).
- **Resolved by architecture, not by porting it**: the custom Icecast-KH
  fork (`ghcr.io/azuracast/icecast-ac`) the Docker build used to pull as
  a prebuilt Linux image was previously this project's highest-risk open
  item (building/porting a Docker/Linux-only binary to FreeBSD). That
  risk no longer applies — this fork runs Icecast in externally-managed,
  per-station FreeBSD jails (see `freebsd/icecast/` below) rather than
  building/bundling it at all, so there's nothing to port. The Dockerfile
  stage pulling that image has been removed. Any standard Icecast build
  (stock `audio/icecast` from ports, or your own) works — AzuraCast no
  longer assumes or requires the AzuraCast-specific patches.
- [`freebsd/icecast/`](icecast/README.md) makes an already-Icecast-
  installed per-station jail (e.g. the existing `skydancer` jail)
  remotely manageable by AzuraCast's PHP side via a supervisord
  `[inet_http_server]` TCP listener. It is a template/pattern applied
  once per station's Icecast jail, not a fixed single-jail config like
  `mariadb`/`webapp`. **Process ownership**: AzuraCast's generated
  supervisord config is the canonical way Icecast starts/stops/restarts
  on that jail once this is applied — do not *also* enable Icecast via
  the jail's own rc.d (e.g. no `icecast_enable=YES`), or you'll get two
  independent Icecast processes fighting over the same port. The
  Icecast *binary* still needs to be installed on that jail some way
  (`pkg install icecast` or your own build) — this scaffold only wires
  up remote process management, it doesn't install Icecast itself.
