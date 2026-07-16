# Icecast jail supervisord template (FreeBSD, native)

**This is a template/pattern, not a single fixed jail config.** Unlike
[`freebsd/mariadb/`](../mariadb/README.md) and
[`freebsd/webapp/`](../webapp/README.md) — each of which sets up exactly
one, fixed-identity jail — Icecast in this project is **one jail per
station**. The user already runs an existing `skydancer` jail as one
station's Icecast instance, and will add more per-station jails (e.g.
`entanglements`) under `/jails/radio/` as more stations are added. This
directory is a pattern you apply once per station's Icecast jail, not a
single stanza you render once for the whole host.

**This does NOT install or configure Icecast itself.** Every file here
assumes Icecast — any standard build (`pkg install icecast` from ports,
or your own) — is *already installed and running* in the target jail
some other way, however you got `skydancer` running today. This fork no
longer needs or assumes AzuraCast's Docker build's patched Icecast-KH
fork at all (that dependency was removed once Icecast moved to
externally-managed per-station jails — see
[`freebsd/README.md`](../README.md)'s "Known open items"). Installing
the Icecast binary itself is still explicitly **out of scope** here —
all this scaffold does is make an *already-running* Icecast jail's
process remotely manageable by
AzuraCast's PHP side, by exposing its supervisord over TCP.

## The remote-management contract this satisfies

Read `backend/src/Radio/RemoteSupervisorClientFactory.php` and
`backend/src/Radio/Frontend/Icecast.php` (`getSupervisor()`,
`DEFAULT_SUPERVISOR_PORT`) for the authoritative source, but in short:
when a station's `frontend_config.host` is set, AzuraCast's PHP side
builds an XML-RPC client pointed at
`http://<host>:<port>/RPC2`, with optional HTTP Basic auth
(`supervisor_username` / `supervisor_password`), defaulting to port
`9002` (`Icecast::DEFAULT_SUPERVISOR_PORT`) if `supervisor_port` is unset.
Everything in this directory exists to put a supervisord on the other end
of that URL, listening on the station's Icecast jail.

## Files here

| File | Purpose |
|---|---|
| `00-install-supervisor.sh` | Installs supervisord (via `pip`, mirroring `freebsd/webapp/20-supervisor.sh` exactly) into the target Icecast jail. Does not touch Icecast. |
| `supervisord.conf.tmpl` | Template (mirroring `freebsd/webapp/supervisord.conf`'s structure) with a new `[inet_http_server]` section for remote management, plus `[unix_http_server]`, `[supervisord]`, `[rpcinterface:supervisor]`, `[supervisorctl]`, and an `[include]` for this station's frontend program stanza. No `[program:icecast]` stanza — see "What this template deliberately does not contain" below. |
| `icecast.env.example` | Tracked placeholder for the per-jail values `render-supervisord-conf.sh` needs (IP, port, username, password). Copy to `icecast.env` and fill in real values per jail — `icecast.env` itself is `.gitignore`d. |
| `render-supervisord-conf.sh` | Renders `supervisord.conf.tmpl` into a concrete `supervisord.conf`, substituting `@@TOKEN@@` placeholders via `sed` (same mechanism `freebsd/mariadb/00-install.sh` uses for `my.cnf.tmpl`). |

## Applying this template to an existing Icecast jail

Worked example below uses the existing `skydancer` jail, matching this
project's established example-jail naming (see `freebsd/README.md`) —
but every step applies verbatim to any other station's Icecast jail
(`entanglements`, or any future one) by substituting its own IP/name.

1. **Install supervisord into the jail** (it isn't there yet, since this
   jail was never set up by `freebsd/webapp/20-supervisor.sh`):

   ```sh
   # from the host, or after copying this directory into the jail:
   jexec skydancer sh -c "$(cat freebsd/icecast/00-install-supervisor.sh)"
   ```

   (or copy `00-install-supervisor.sh` into the jail and run it there
   directly — whichever fits your existing workflow for touching
   `skydancer`.)

2. **Fill in this jail's per-jail values:**

   ```sh
   cp freebsd/icecast/icecast.env.example freebsd/icecast/icecast.env
   ```

   Edit `icecast.env`: set `ICECAST_JAIL_IP` to `skydancer`'s actual
   address, leave `ICECAST_SUPERVISOR_PORT` at `9002` unless you have a
   reason to change it, set `ICECAST_SUPERVISOR_USERNAME`, and set
   `ICECAST_SUPERVISOR_PASSWORD` to a real, strong password (the script
   refuses to render with the shipped placeholder left in place, same
   guard `freebsd/mariadb/00-install.sh`'s DB password uses).

3. **Render `supervisord.conf`:**

   ```sh
   sh freebsd/icecast/render-supervisord-conf.sh
   ```

   This is a per-jail, per-run render step, not something wired into the
   shared `freebsd/env.conf` / `generate-jail-conf.sh` machinery — that
   machinery is scoped to this host's fixed single-instance jails
   (`mariadb`, `webapp`), and doesn't fit a value that's different for
   every station. Re-run this any time you change `icecast.env` (e.g. to
   rotate the password) or add another station's jail (with a different
   `icecast.env`, or the equivalent CLI args, per jail).

4. **Install the rendered config into the jail:**

   ```sh
   cp freebsd/icecast/supervisord.conf /usr/local/etc/supervisord.conf   # inside the jail
   ```

5. **Set up the shared mount** so this jail's supervisord can see the
   station's dynamically-generated `supervisord.frontend.conf` — see
   "Shared mount requirement" below. Do this before starting supervisord,
   or its `[include]` glob will just match nothing until the mount and
   the first `writeConfiguration()` call both exist.

6. **Start supervisord in the jail.** `00-install-supervisor.sh` installs
   the FreeBSD port (`py312-supervisor`), which ships a native rc.d
   service, and enables it (`sysrc supervisord_enable=YES`) — so:

   ```sh
   service supervisord start   # inside the jail
   ```

   and it starts automatically on every jail boot thereafter. (The port's
   rc.d default config path is `/usr/local/etc/supervisord.conf`, exactly
   where step 4 installed the rendered file — no extra rcvar needed.)

7. **Point the station at it**, in the AzuraCast admin UI (webapp jail),
   on this station's frontend config:
   - `host` = `skydancer`'s IP (same value as `ICECAST_JAIL_IP` above)
   - `supervisor_port` = same value as `ICECAST_SUPERVISOR_PORT` (9002
     by default)
   - `supervisor_username` / `supervisor_password` = same values as
     `ICECAST_SUPERVISOR_USERNAME` / `ICECAST_SUPERVISOR_PASSWORD`

   These four fields are exactly what `Icecast::getSupervisor()` reads
   to build the remote client — get them to match exactly or AzuraCast
   won't be able to reach this jail's supervisord.

Repeat steps 1–7 (with a different `icecast.env`, or different CLI args)
for every additional station's Icecast jail.

## What this template deliberately does not contain

- **No `[program:icecast]` stanza.** `Configuration::writeConfiguration()`
  generates the actual frontend program stanza dynamically, per station,
  via `buildAdapterSupervisorConfig()`, and writes it to
  `supervisord.frontend.conf` (see `Configuration::getSupervisorConfPath()`)
  — not something this static scaffold hardcodes. This template's job ends
  at making that file visible to this jail's supervisord (see `[include]`
  in `supervisord.conf.tmpl`, and "Shared mount requirement" below).
- **No Icecast install/build.** Confirmed above; not revisited here.
- **No wiring into `freebsd/env.conf`.** That file is single-instance-jail
  scoped; see "Applying this template" step 3 for why this uses its own
  small `icecast.env` instead.

## Process ownership — read before applying this to a jail with Icecast already rc.d-enabled

Once this template is applied and `Configuration.php`'s generated
`supervisord.frontend.conf` is visible to this jail's supervisord (via
the `[include]` glob + shared mount), **supervisord becomes the thing
that starts/stops/restarts Icecast on this jail** — not the jail's own
rc.d. If Icecast is currently enabled via `sysrc icecast_enable=YES`
(or started manually at boot some other way) on a jail you're applying
this to, disable that first. Running both simultaneously means two
independent Icecast processes fighting over the same port — not
integration, and not something supervisord or AzuraCast will detect or
warn you about on its own.

## Shared mount requirement

This is not a new decision introduced by this change — it's a design
consequence already implied by the project's existing split-file
supervisord approach (see the comment block in
`freebsd/webapp/supervisord.conf` about `supervisord.{backend,frontend}.conf`
being written "including when frontend runs in a separate jail from
backend"), just made concrete here.

The PHP application (running in the **webapp** jail) writes each
station's `supervisord.frontend.conf` to
`<station radio_base_dir>/config/supervisord.frontend.conf` — by default
`/var/azuracast/stations/<short_name>/config/supervisord.frontend.conf`
(`Station::getRadioConfigDir()` = `radio_base_dir . '/config'`;
`radio_base_dir` defaults to `Environment::getStationDirectory() .
'/' . short_name`, i.e. `/var/azuracast/stations/<short_name>`). This
jail's supervisord (via the `[include]` glob in `supervisord.conf.tmpl`)
needs to see that exact file — but nothing in the **Icecast** jail runs
the PHP app, so nothing writes it there locally. The file has to be
shared between the two jails' filesystems.

Also into that same directory, this jail's own Icecast process itself
reads/writes several files directly (see `Frontend\Icecast.php`):
- reads `icecast.xml` (`getConfigurationPath()`, written by the webapp
  side) as its config file,
- writes `icecast.pid` (its `pidfile`),
- writes `icecast_access.log` and `icecast.log` (`accesslog`/`errorlog`).

Because Icecast itself needs to **write** into this directory (pidfile,
logs), the mount must be **read-write**, not read-only.

**Recommended mechanism:** a FreeBSD jail(8) `mount` parameter (fstab-line
syntax) added to this station's Icecast jail's existing `jail.conf`
stanza — `freebsd/jail.conf.d/*.conf.tmpl` don't currently use `mount` for
anything (neither `mariadb` nor `webapp` needs a shared filesystem), so
there's no existing in-tree convention to match; this follows jail(8)'s
own documented `mount` parameter syntax instead, which is mounted/unmounted
automatically alongside the jail's own start/stop (unlike an ad hoc
`exec.start`/`exec.poststop` `mount`/`umount` pair, which you'd have to
get exactly right yourself):

```
skydancer {
    ...                                                   # existing stanza, unchanged

    mount += "/var/azuracast/stations/skydancer/config /jails/radio/skydancer/var/azuracast/stations/skydancer/config nullfs rw 0 0";
}
```

Notes on the exact paths above:
- The **source** path (`/var/azuracast/stations/skydancer/config`) is a
  path *inside the webapp jail's own filesystem* (`WEBAPP_JAIL_PATH` in
  `freebsd/env.conf` + that station's config dir) — adjust it to wherever
  the AzuraCast app is actually deployed and this station's `short_name`
  really is.
- The **destination** path is the *same absolute path*
  (`/var/azuracast/stations/skydancer/config`), just rooted under this
  Icecast jail's own `path` (e.g. `/jails/radio/skydancer/...`) — kept
  identical on purpose so `[include]`'s glob and Icecast's own
  `getConfigurationPath()`/log paths need no translation between the two
  jails' views of "this station's config dir".
- `rw` (not `ro`) because Icecast itself writes its pidfile/logs here, as
  above.
- Per this project's one-jail-per-station design, only **this one
  station's** config directory is mounted into this jail — not the whole
  `/var/azuracast/stations/*` tree — so one Icecast jail never sees
  another station's files. `supervisord.conf.tmpl`'s `[include]` glob
  still uses `*` (for parity with `webapp`'s own glob), but in practice
  only ever matches the single directory actually mounted in.
- Confirm `webapp`'s host-side jail root path (`WEBAPP_JAIL_PATH` in
  `freebsd/env.conf`) and this station's actual `short_name` before using
  the line above verbatim — the path shown is illustrative, following the
  `skydancer` example.

## Known open questions / judgment calls

- **`azuracast` system user:** `freebsd/webapp/20-supervisor.sh` chowns
  its log dir to `azuracast:azuracast`, a user its own `00-packages.sh`
  creates. This jail was never set up by this project, so
  `00-install-supervisor.sh` leaves `/var/log/azuracast` root-owned by
  default rather than assuming that user exists here — adjust by hand if
  your Icecast jail happens to already have a matching user/group.
- **rc.d comes from the port, not this template.** `00-install-supervisor.sh`
  installs `py312-supervisor` from ports specifically because it ships a
  native `rc.d/supervisord` service (enabled by the script via
  `sysrc supervisord_enable=YES`). This deliberately differs from
  `webapp/20-supervisor.sh`'s pip install: the webapp jail must NOT have
  an independently-enabled supervisord service, because its own
  `rc.d/azuracast` owns supervisord's lifecycle there (MariaDB-wait +
  migrations first) — two services starting the same supervisord means
  two instances fighting over the same programs, a failure mode confirmed
  on a real install.
- **This was not tested against a real jail** as part of this change —
  same caveat as the rest of `freebsd/` (see `freebsd/README.md`'s "Known
  open items"). Treat this as a carefully reviewed first draft, checked
  by hand for consistency against `freebsd/mariadb/` and `freebsd/webapp/`
  conventions, not a verified install.
- **Icecast itself is still not installed by this directory** — it only
  solves the separate "make an already-running Icecast remotely
  supervisord-manageable" problem. Unlike earlier drafts of this
  project's planning, this is no longer a high-risk item: any standard
  Icecast build works (see `freebsd/README.md`'s "Known open items"),
  there's no Docker/Linux-only fork to port anymore.
