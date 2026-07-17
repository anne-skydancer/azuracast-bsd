#!/bin/sh
#
# freebsd/install.sh
#
# Guided installer for azuracast-bsd on a FreeBSD host. Run AS ROOT on
# the HOST (not inside a jail); it creates/provisions the jail(s) and
# drives the per-component scripts in this tree in the right order,
# pausing for confirmation at each stage so you can inspect or bail.
#
# Two topologies:
#
#   --mode distributed   The reference deployment: a `mariadb` jail, a
#                        `webapp` jail (nginx/php/engine/etc.), and one
#                        Icecast jail per station (those remain a
#                        manual, per-station template -- see
#                        freebsd/icecast/README.md -- because they are
#                        typically pre-existing jails this installer
#                        must not reshape). Battle-tested layout.
#
#   --mode integrated    Everything -- MariaDB, the web stack, the
#                        streaming engine, AND Icecast -- inside ONE
#                        jail. Simpler to run, no cross-jail mounts or
#                        remote supervisord; the trade is no
#                        per-component blast-radius isolation. (For
#                        future reference: the planned Linux/Docker
#                        sister project maps to this topology only.)
#
# Prerequisites either way:
#   - Edit freebsd/env.conf FIRST (every IP/path/hostname; shipped
#     values are IETF documentation placeholders and will not work).
#   - A vm-public style bridge for jail VNET interfaces (see env.conf).
#
# What this script does NOT do:
#   - Touch per-station Icecast jails (distributed mode) beyond printing
#     the pointer to freebsd/icecast/README.md.
#   - Configure your router/firewall, DNS, or ACME certificates.
#   - Anything without asking first.

set -eu

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
REPO_DIR=$(CDPATH= cd -- "${SCRIPT_DIR}/.." && pwd)

MODE=""

usage() {
    sed -n '2,40p' "$0" | sed 's/^# \{0,1\}//'
    exit "${1:-0}"
}

while [ $# -gt 0 ]; do
    case "$1" in
        --mode) MODE="${2:?--mode needs an argument}"; shift 2 ;;
        --mode=*) MODE="${1#--mode=}"; shift ;;
        -h|--help) usage 0 ;;
        *) echo "Unknown argument: $1" >&2; usage 1 ;;
    esac
done

# --- helpers ---------------------------------------------------------------

msg()  { printf '\n\033[1m==> %s\033[0m\n' "$*"; }
note() { printf '    %s\n' "$*"; }
die()  { printf 'ERROR: %s\n' "$*" >&2; exit 1; }

confirm() {
    # confirm <prompt> -- returns 0 on yes, exits on no. Every state
    # change goes through here: this is a guided installer, not a
    # fire-and-forget one.
    printf '%s [y/N] ' "$1"
    read -r _ans
    case "$_ans" in
        y|Y|yes|YES) return 0 ;;
        *) die "aborted by operator." ;;
    esac
}

ask() {
    # ask <prompt> <default> -- prints the answer (default if empty).
    printf '%s [%s]: ' "$1" "$2" >&2
    read -r _ans
    if [ -n "$_ans" ]; then printf '%s' "$_ans"; else printf '%s' "$2"; fi
}

ask_secret() {
    # ask_secret <prompt> -- prompts twice with echo off, prints the value.
    while :; do
        printf '%s: ' "$1" >&2
        stty -echo; read -r _s1; stty echo; printf '\n' >&2
        printf 'Repeat to confirm: ' >&2
        stty -echo; read -r _s2; stty echo; printf '\n' >&2
        if [ -z "$_s1" ]; then printf 'Must not be empty.\n' >&2; continue; fi
        if [ "$_s1" = "$_s2" ]; then printf '%s' "$_s1"; return 0; fi
        printf 'Mismatch, try again.\n' >&2
    done
}

in_jail() {
    # in_jail <jail-name> <command...>
    _j="$1"; shift
    jexec "$_j" "$@"
}

in_jail_sh() {
    # in_jail_sh <jail-name> <shell-snippet> -- runs via /bin/sh -c so
    # redirects/globs behave regardless of the host operator's shell
    # (learned the hard way under tcsh).
    _j="$1"; shift
    jexec "$_j" /bin/sh -c "$*"
}

# --- preflight ---------------------------------------------------------------

[ "$(id -u)" = "0" ] || die "run as root on the FreeBSD host."
[ "$(uname -s)" = "FreeBSD" ] || die "this installer is for FreeBSD hosts."
[ -f "${SCRIPT_DIR}/env.conf" ] || die "freebsd/env.conf not found."

. "${SCRIPT_DIR}/env.conf"

# Refuse to run against the shipped IETF documentation placeholders --
# they are guaranteed unroutable and mean env.conf was never edited.
case "${WEBAPP_JAIL_IP}${INTEGRATED_JAIL_IP}" in
    *192.0.2.*)
        die "freebsd/env.conf still contains its shipped placeholder addresses
       (192.0.2.x / 2001:db8:: are IETF documentation ranges and will not
       work on any real network). Edit env.conf to match YOUR jail/network
       layout first -- see freebsd/README.md." ;;
esac

if [ -z "$MODE" ]; then
    printf '\nChoose installation topology:\n'
    printf '  1) distributed -- mariadb jail + webapp jail + per-station Icecast jails (reference deployment)\n'
    printf '  2) integrated  -- everything in one jail\n'
    printf 'Selection [1/2]: '
    read -r _sel
    case "$_sel" in
        1) MODE=distributed ;;
        2) MODE=integrated ;;
        *) die "pick 1 or 2." ;;
    esac
fi
case "$MODE" in distributed|integrated) : ;; *) die "--mode must be 'distributed' or 'integrated'." ;; esac

msg "Mode: ${MODE}"

# --- shared building blocks --------------------------------------------------

ensure_rootfs() {
    # ensure_rootfs <jail-path> <jail-name> -- if the jail directory has
    # no userland yet, offer to fetch/extract base.txz for this host's
    # release. Cached download so multiple jails extract from one fetch.
    _path="$1"; _name="$2"
    if [ -x "${_path}/bin/sh" ]; then
        note "jail rootfs already present at ${_path}"
        return 0
    fi
    _rel=$(uname -r | sed 's/-p[0-9]*$//')
    _arch=$(uname -m)
    _cache="/var/cache/azuracast-bsd"
    _txz="${_cache}/base-${_rel}-${_arch}.txz"
    msg "No userland found at ${_path} for jail '${_name}'."
    confirm "Fetch FreeBSD ${_rel}/${_arch} base.txz and extract it there?"
    mkdir -p "${_cache}" "${_path}"
    if [ ! -f "${_txz}" ]; then
        fetch -o "${_txz}" "https://download.freebsd.org/releases/${_arch}/${_rel}/base.txz"
    fi
    tar -xpf "${_txz}" -C "${_path}"
    # Minimal in-jail plumbing every jail needs.
    cp /etc/resolv.conf "${_path}/etc/resolv.conf"
    if [ -f /etc/localtime ]; then cp /etc/localtime "${_path}/etc/localtime"; fi
    touch "${_path}/etc/rc.conf"
    note "rootfs extracted to ${_path}"
}

install_stanza() {
    # install_stanza <name> -- renders jail.conf.d/<name>.conf (already
    # generated) into the host's jail configuration. Uses
    # /etc/jail.conf.d/ when the host has it; otherwise asks the
    # operator to merge by hand (never blind-append to /etc/jail.conf).
    _name="$1"
    _src="${SCRIPT_DIR}/jail.conf.d/${_name}.conf"
    [ -f "$_src" ] || die "missing ${_src} -- generate-jail-conf.sh failed?"
    if [ -d /etc/jail.conf.d ]; then
        cp "$_src" "/etc/jail.conf.d/${_name}.conf"
        note "installed /etc/jail.conf.d/${_name}.conf"
    else
        msg "Your host has no /etc/jail.conf.d -- merge this stanza into /etc/jail.conf yourself:"
        note "$_src"
        confirm "Done merging (or it was already there)?"
    fi
}

copy_setup_tree() {
    # copy_setup_tree <jail-path> -- makes this repo's freebsd/ scripts
    # runnable inside the jail before the app itself is cloned there
    # (bootstrap chicken-and-egg: the scripts install git).
    _path="$1"
    rm -rf "${_path}/tmp/azuracast-setup"
    mkdir -p "${_path}/tmp/azuracast-setup"
    cp -R "${SCRIPT_DIR}/." "${_path}/tmp/azuracast-setup/"
}

start_jail() {
    _name="$1"
    if jls -j "$_name" >/dev/null 2>&1; then
        note "jail '${_name}' already running"
    else
        service jail start "$_name" || die "jail '${_name}' failed to start -- check /var/log/messages and the stanza."
    fi
}

self_signed_placeholder() {
    # self_signed_placeholder <jail-name> -- nginx.conf references the
    # ACME cert paths at startup; before any real certificate exists,
    # nginx fails config validation without SOMETHING there. Real certs
    # come later via the app's built-in ACME support and simply replace
    # these (confirmed necessary on the reference install).
    _j="$1"
    in_jail_sh "$_j" '
        if [ ! -f /var/azuracast/storage/acme/ssl.crt ]; then
            mkdir -p /var/azuracast/storage/acme
            openssl req -x509 -newkey rsa:2048 -nodes -days 3650 \
                -keyout /var/azuracast/storage/acme/ssl.key \
                -out /var/azuracast/storage/acme/ssl.crt \
                -subj "/CN=placeholder.invalid" >/dev/null 2>&1
            chown -R azuracast:azuracast /var/azuracast/storage/acme
            echo "self-signed placeholder certificate created"
        fi'
}

provision_webapp_stack() {
    # provision_webapp_stack <jail-name> -- freebsd/webapp/README.md's
    # setup order, steps 1-7, executed instead of narrated. Works for
    # both topologies (in integrated mode the same jail also gets
    # MariaDB/Icecast via provision_integrated_extras).
    _j="$1"
    _setup="/tmp/azuracast-setup"

    msg "[${_j}] installing base packages (00-packages.sh -- this takes a while)"
    in_jail_sh "$_j" "sh ${_setup}/webapp/00-packages.sh"

    msg "[${_j}] building Centrifugo from source (10-centrifugo.sh)"
    in_jail_sh "$_j" "sh ${_setup}/webapp/10-centrifugo.sh"
    in_jail_sh "$_j" "cp ${_setup}/webapp/centrifugo-config.toml /var/azuracast/centrifugo/config.toml"

    msg "[${_j}] building SFTPGo from source (11-sftpgo.sh)"
    in_jail_sh "$_j" "sh ${_setup}/webapp/11-sftpgo.sh"
    in_jail_sh "$_j" "cp ${_setup}/webapp/sftpgo.json /var/azuracast/sftpgo/sftpgo.json"
    in_jail_sh "$_j" '
        for t in rsa ecdsa ed25519; do
            f="/var/azuracast/storage/sftpgo/id_${t}"
            [ -f "$f" ] && continue
            case "$t" in
                rsa)   ssh-keygen -t rsa -b 4096 -f "$f" -q -N "" ;;
                ecdsa) ssh-keygen -t ecdsa -b 521 -f "$f" -q -N "" ;;
                *)     ssh-keygen -t ed25519 -f "$f" -q -N "" ;;
            esac
        done
        chown -R azuracast:azuracast /var/azuracast/storage/sftpgo'

    msg "[${_j}] installing supervisord (20-supervisor.sh) + base config"
    in_jail_sh "$_j" "sh ${_setup}/webapp/20-supervisor.sh"
    in_jail_sh "$_j" "cp ${_setup}/webapp/supervisord.conf /usr/local/etc/supervisord.conf"

    msg "[${_j}] installing nginx + php-fpm pool configs"
    in_jail_sh "$_j" "cp ${_setup}/webapp/nginx.conf /usr/local/etc/nginx/nginx.conf
        cp ${_setup}/webapp/nginx-proxy_params /usr/local/etc/nginx/proxy_params
        cp ${_setup}/webapp/php-fpm.d/www.conf /usr/local/etc/php-fpm.d/www.conf
        cp ${_setup}/webapp/php-fpm.d/internal.conf /usr/local/etc/php-fpm.d/internal.conf"
    # The M3U8 MIME fix Docker's nginx.sh applies (HLS playlists).
    in_jail_sh "$_j" "sed -i '' 's|application/vnd.apple.mpegurl|application/x-mpegurl|' /usr/local/etc/nginx/mime.types || true"

    msg "[${_j}] installing rc.d script + crontab + Valkey"
    in_jail_sh "$_j" "cp ${_setup}/webapp/rc.d/azuracast /usr/local/etc/rc.d/azuracast
        chmod +x /usr/local/etc/rc.d/azuracast
        sysrc azuracast_enable=YES
        mkdir -p /etc/rc.conf.d
        echo 'azuracast_path=\"/var/azuracast/www\"' > /etc/rc.conf.d/azuracast
        crontab -u azuracast ${_setup}/webapp/crontab
        sysrc valkey_enable=YES
        cp ${_setup}/webapp/valkey.conf /usr/local/etc/valkey.conf
        service valkey start || true"

    self_signed_placeholder "$_j"
}

deploy_app() {
    # deploy_app <jail-name> -- INSTALL.md steps 5-6: clone to the
    # canonical (non-negotiable) path, build PHP/JS.
    _j="$1"
    _url=$(ask "Git URL of your azuracast-bsd fork" "https://github.com/anne-skydancer/azuracast-bsd.git")
    msg "[${_j}] cloning ${_url} to /var/azuracast/www (path is NOT a free choice -- see INSTALL.md step 5)"
    in_jail_sh "$_j" "
        if [ -d /var/azuracast/www/.git ]; then
            echo 'checkout already present, skipping clone'
        else
            git clone '${_url}' /var/azuracast/www
        fi
        chown -R azuracast:azuracast /var/azuracast/www
        git config --global --add safe.directory /var/azuracast/www"

    msg "[${_j}] composer + frontend build (long)"
    in_jail_sh "$_j" "cd /var/azuracast/www && composer install --no-dev --no-ansi --no-interaction && composer dump-autoload --optimize --classmap-authoritative"
    in_jail_sh "$_j" "cd /var/azuracast/www && npm ci --include=dev && npm run build"
}

build_engine() {
    _j="$1"
    msg "[${_j}] building the Rust streaming engine (first build fetches the toolchain -- long)"
    in_jail_sh "$_j" "export AZURACAST_PATH=/var/azuracast/www; sh /var/azuracast/www/freebsd/webapp/build-engine.sh"
}

# --- distributed mode --------------------------------------------------------

install_distributed() {
    msg "Rendering jail stanzas from env.conf"
    sh "${SCRIPT_DIR}/generate-jail-conf.sh"
    install_stanza mariadb
    install_stanza webapp

    ensure_rootfs "${MARIADB_JAIL_PATH}" "${MARIADB_JAIL_NAME}"
    ensure_rootfs "${WEBAPP_JAIL_PATH}" "${WEBAPP_JAIL_NAME}"

    confirm "Start jails '${MARIADB_JAIL_NAME}' and '${WEBAPP_JAIL_NAME}' now?"
    start_jail "${MARIADB_JAIL_NAME}"
    start_jail "${WEBAPP_JAIL_NAME}"

    msg "Provisioning the ${MARIADB_JAIL_NAME} jail"
    copy_setup_tree "${MARIADB_JAIL_PATH}"
    in_jail_sh "${MARIADB_JAIL_NAME}" "sh /tmp/azuracast-setup/mariadb/00-install.sh"
    DB_PASS=$(ask_secret "Choose the AzuraCast database password (you will enter the SAME one again in the DB-config step)")
    in_jail_sh "${MARIADB_JAIL_NAME}" "export AZURACAST_DB_PASSWORD='${DB_PASS}'; sh /tmp/azuracast-setup/mariadb/10-provision-db.sh"

    msg "Provisioning the ${WEBAPP_JAIL_NAME} jail"
    copy_setup_tree "${WEBAPP_JAIL_PATH}"
    provision_webapp_stack "${WEBAPP_JAIL_NAME}"
    deploy_app "${WEBAPP_JAIL_NAME}"

    msg "Database connection config (interactive -- when asked, the default topology answers are correct; use the password you set above)"
    in_jail "${WEBAPP_JAIL_NAME}" /bin/sh -c "export AZURACAST_PATH=/var/azuracast/www; sh /var/azuracast/www/freebsd/webapp/configure-db.sh"

    build_engine "${WEBAPP_JAIL_NAME}"

    confirm "Start AzuraCast (service azuracast start in ${WEBAPP_JAIL_NAME})?"
    in_jail "${WEBAPP_JAIL_NAME}" service azuracast start

    msg "Distributed install complete."
    note "Web UI: http://${WEBAPP_JAIL_IP}/ (create your admin account there)."
    note ""
    note "Per-station Icecast jails are deliberately NOT touched by this installer:"
    note "follow freebsd/icecast/README.md (steps 1-10, including the watchdog,"
    note "the ACME cert nullfs mount, and the station fallback file) for each one."
}

# --- integrated mode ----------------------------------------------------------

install_integrated() {
    msg "Rendering jail stanzas from env.conf"
    sh "${SCRIPT_DIR}/generate-jail-conf.sh"
    install_stanza integrated

    ensure_rootfs "${INTEGRATED_JAIL_PATH}" "${INTEGRATED_JAIL_NAME}"

    confirm "Start jail '${INTEGRATED_JAIL_NAME}' now?"
    start_jail "${INTEGRATED_JAIL_NAME}"

    copy_setup_tree "${INTEGRATED_JAIL_PATH}"
    _j="${INTEGRATED_JAIL_NAME}"

    provision_webapp_stack "$_j"

    msg "[${_j}] installing MariaDB (local, loopback-only) + Icecast"
    in_jail_sh "$_j" "pkg install -y mariadb118-server icecast mime-support
        sysrc mysql_enable=YES
        mkdir -p /usr/local/etc/mysql/conf.d
        cp /tmp/azuracast-setup/integrated/my.cnf /usr/local/etc/mysql/conf.d/azuracast.cnf
        [ -e /etc/mime.types ] || ln -s /usr/local/etc/mime.types /etc/mime.types"

    DB_PASS=$(ask_secret "Choose the AzuraCast database password (you will enter the SAME one again in the DB-config step)")
    in_jail_sh "$_j" "export AZURACAST_DB_PASSWORD='${DB_PASS}'; sh /tmp/azuracast-setup/integrated/10-provision-db-local.sh"

    # In this topology the SAME supervisord runs the station frontend
    # (Icecast) programs too, so widen the include glob that the
    # distributed webapp config deliberately narrows (its comment
    # explains the narrowing; this is the co-located inverse).
    msg "[${_j}] widening supervisord include for co-located Icecast frontends"
    in_jail_sh "$_j" '
        f=/usr/local/etc/supervisord.conf
        want="/var/azuracast/stations/*/config/supervisord.frontend.conf"
        if grep -q "supervisord.frontend.conf" "$f"; then
            echo "include already widened"
        elif grep -q "supervisord.backend.conf" "$f"; then
            sed -i "" "s|supervisord.backend.conf|supervisord.backend.conf ${want}|" "$f"
            grep -q "supervisord.frontend.conf" "$f" || { echo "sed failed to widen include" >&2; exit 1; }
        else
            echo "unexpected supervisord.conf include line -- widen it by hand" >&2
            exit 1
        fi'

    deploy_app "$_j"

    msg "Database connection config (interactive). Answer YES to 'existing server', host 127.0.0.1, port 3306, and the password you set above."
    in_jail "$_j" /bin/sh -c "export AZURACAST_PATH=/var/azuracast/www; sh /var/azuracast/www/freebsd/webapp/configure-db.sh"

    build_engine "$_j"

    confirm "Start AzuraCast (service azuracast start in ${_j})?"
    in_jail "$_j" service azuracast start

    msg "Integrated install complete."
    note "Web UI: http://${INTEGRATED_JAIL_IP}/ (create your admin account there)."
    note ""
    note "Notes specific to this topology:"
    note " - When creating a station, leave the frontend 'Broadcasting host' blank:"
    note "   Icecast runs co-located and defaults to 127.0.0.1."
    note " - Install a fallback file per mount into /usr/local/share/icecast/web/"
    note "   (see freebsd/icecast/README.md step 10 -- same recipe, same jail here)."
    note " - The Icecast wedge watchdog (freebsd/icecast/watchdog.sh) works here"
    note "   unchanged if you want it: install to /usr/local/sbin/icecast-watchdog"
    note "   and add the crontab line from its header."
}

# --- go ------------------------------------------------------------------------

if [ "$MODE" = "distributed" ]; then
    install_distributed
else
    install_integrated
fi
