# AzuraCast on FreeBSD jails (no Docker)

This directory replaces AzuraCast's Docker Compose stack with native
FreeBSD jails, sharing your host's `jail.conf` schema (VNET + `epairN`,
static internal IPv4/IPv6, default routes via `DEFAULT_ROUTE_V4` in
`env.conf`).

Two topologies are supported — **distributed** (a `mariadb` jail + a
`webapp` jail + one Icecast jail per station; the reference deployment,
described throughout this directory) and **integrated** (everything in
one jail; see `integrated/`). A guided installer drives either:

```sh
sh freebsd/install.sh            # prompts for the topology
```

See `INSTALL.md` at the repo root for the topology trade-offs and the
full manual walkthrough the installer automates.

## Before you do anything else

Every IP, hostname, path, and epair interface used across this whole
`freebsd/` tree lives in one place: `freebsd/env.conf` — which you
create by copying the tracked template:

```sh
cp freebsd/env.conf.example freebsd/env.conf
```

Your copy is `.gitignore`d, so your real topology never collides with
upstream changes to the template (a lesson from the reference install's
first `git pull` conflict).
**The values shipped in the template are not real** — they're IETF
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
| one jail per station | (existing, per-station, whatever you've named them) | Audio frontend (Icecast) | Stock `audio/icecast` from ports works — the custom Icecast-KH fork Docker used is not needed (see "Known open items" below). [`freebsd/icecast/`](icecast/README.md) is a *template* (not a fixed single jail like the two rows above) that makes an already-running Icecast jail's supervisord remotely manageable by AzuraCast's PHP side — it does not install or build Icecast itself. |

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
4. Deploy the AzuraCast PHP application itself into `webapp` — follow
   [`INSTALL.md`](../INSTALL.md) steps 5–8 (clone to **exactly**
   `/var/azuracast/www` — the path is not a free choice, see that
   step's explanation — then composer/npm builds, DB configuration,
   and `freebsd/webapp/build-engine.sh` for the Rust streaming engine
   binary, without which no station can start).
5. Confirm `webapp` can reach `mariadb` on the value of `MARIADB_PORT` in
   `env.conf`. Restricting network
   access beyond that (via `pf` or otherwise) is your own firewall
   policy to set up if you want it — out of scope for this project,
   which relies on jail isolation as the actual security boundary.

## The streaming engine

Each station's backend is this fork's own Rust streaming engine
(`engine/` at the repo root — decode, crossfade, replaygain, live-DJ
harbor, HLS, Icecast source output, in-stream metadata), built and
installed by `freebsd/webapp/build-engine.sh`. Liquidsoap is gone
entirely; there is nothing OCaml anywhere in this stack. See
`engine/SPEC.md` for the behavioral contract it was built against and
`engine/README.md` for its architecture.

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

## Media mirror (NAS-independent playback)

If the music library lives on a NAS, don't feed the NFS mount to
AzuraCast directly — the reference deployment's NAS reboots far slower
than the server after power cuts, which meant a cold boot played the
error jingle until the NAS appeared (and the `soft` NFS mount was the
standing suspect for occasional mid-track skips). Instead, keep a local
mirror on the host and nullfs-mount *that* into the jail:

```
NAS (source of truth) --rsync, cron--> local mirror --nullfs--> jail media path
```

Where the library lives is AzuraCast's own concern, and it already has
answers: every station automatically gets (and registers)
`/var/azuracast/stations/<short_name>/media` at creation — web/SFTP
uploads land there with zero setup — and a *custom storage location*
(any path, registerable in the UI, shareable across stations) covers
externally-supplied libraries. The mirror below is just one way to
supply such a path.

[`media-mirror-sync.sh`](media-mirror-sync.sh) is the host-side sync
script — sentinel-guarded so an unmounted NAS can never trick
`rsync --delete` into emptying the mirror; its header is the full
install procedure (rsync package, cron line with `lockf`, first-sync,
and repointing the jail's media mount). Playback then survives any NAS
outage indefinitely; new music appears one cron cycle after you add it
on the NAS.

## Known open items

- This tree has been exercised end-to-end on a real FreeBSD 15.1 host
  (2026-07) — a station built from it is live on air. The confirmed
  fixes from that install are folded into the scripts/configs and
  called out inline (php-fpm pools, the rc.subr `${name}_user`
  collision, proxy_params, SFTPGo's build method, Icecast 2.5-beta
  hardening). Each subdirectory's README still has a "things worth
  double-checking" section for the few points that remain
  environment-dependent (`audiowaveform` has no FreeBSD port, the
  OpenSSL 3.0→3.5 ports transition).
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
