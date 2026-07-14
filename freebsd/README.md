# AzuraCast on FreeBSD jails (no Docker)

This directory replaces AzuraCast's Docker Compose stack with three
native FreeBSD jails on the same host as the existing `skydancer` and
`icecast` jails, sharing their `jail.conf` schema (VNET + `epairN`,
static internal IPv4/IPv6, default routes via `DEFAULT_ROUTE_V4` in
`env.conf`, currently `10.0.0.254`).

## Before you do anything else

Every IP, hostname, path, and epair interface used across this whole
`freebsd/` tree lives in one place: [`freebsd/env.conf`](env.conf).
Review and edit that file to match your own jail/network layout first
(the values shipped there are this project's own reference deployment,
not a universal default), then run:

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
| `mariadb` | value of `MARIADB_JAIL_IP` in `env.conf` (currently `10.8.0.100`) / `::100` | Database only | [`freebsd/mariadb/`](mariadb/README.md) |
| `webapp` | value of `WEBAPP_JAIL_IP` in `env.conf` (currently `10.8.0.110`) / `::110` | nginx, php-fpm, Valkey, Centrifugo, SFTPGo, cron, supervisord | [`freebsd/webapp/`](webapp/README.md) |
| `icecast` | (existing) | Audio frontend | Already set up outside this change — see Phase 1's Risk #1 in the project plan re: the custom Icecast-KH fork |

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
   in `env.conf`, currently `10.8.0.110`), so get the DB up and the pf
   rule in place before starting `webapp`.
3. Provision `webapp` (see `webapp/README.md`).
4. Deploy the AzuraCast PHP application itself into `webapp` (git
   clone/composer install/frontend build) — this isn't covered by
   either subdirectory, which only set up the surrounding OS-level
   platform, not the app deployment step.
5. Confirm `webapp` can reach `mariadb` on port 3306. Restricting network
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

## Known open items

- Neither jail's scripts have been run against a real FreeBSD box as
  part of this change — treat package names, paths, and versions as a
  well-researched first draft. Each subdirectory's README has a "things
  worth double-checking" section calling out specific unconfirmed
  points (a few inferred PHP 8.5 extension package names,
  `audiowaveform` having no FreeBSD port, MariaDB's exact rc.d service
  name).
- The custom Icecast-KH fork (`ghcr.io/azuracast/icecast-ac`) that the
  Docker build currently pulls as a prebuilt Linux image is not
  addressed by this phase at all — it's the highest-risk open item in
  the whole project (see the plan's Risks section) and needs its own
  spike.
