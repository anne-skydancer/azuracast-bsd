#!/bin/sh
#
# freebsd/webapp/00-packages.sh
#
# Base package install + user/directory bootstrap for the `webapp` jail
# (WEBAPP_JAIL_IP/WEBAPP_JAIL_HOSTNAME in freebsd/env.conf). This jail runs
# nginx, php-fpm, Valkey, Centrifugo, SFTPGo, cron, and supervisord. It does
# NOT run MariaDB (separate `mariadb` jail, MARIADB_JAIL_IP in env.conf) or
# Icecast (separate `icecast` jail / out of scope here).
#
# Source-of-truth notes (read before running):
#
#   - The Docker build's `final`/`pre-final` stage is pinned to
#     `FROM php:8.5-fpm-trixie` (Dockerfile line 41) — i.e. PHP **8.5**.
#     This script installs `php85*` packages to match that exactly.
#     Verified against FreshPorts (2026-07): lang/php85 is at 8.5.8_1, a
#     mature stable release (8 patch releases in), not an early/alpha
#     snapshot -- safe to depend on.
#
#   - PHP extensions below are a 1:1 translation of the actual
#     `install-php-extensions` invocation in util/docker/web/setup/php.sh:
#       install-php-extensions @composer \
#         gd curl xml zip \
#         gmp pdo_mysql mbstring intl \
#         redis maxminddb uuid \
#         ffi sockets
#     Notably: only `pdo_mysql` is installed, NOT `mysqli` — AzuraCast's
#     Docker image does not ship mysqli. Do not add it unless you've
#     confirmed the app needs it. Likewise pcntl/bcmath/sysvsem/sysvshm
#     are NOT installed by the Docker build and are deliberately omitted
#     here for fidelity, even though they're available as php85-* ports.
#
#   - Package names below are verified against FreshPorts (2026-07), not
#     inferred:
#       * `php85-composer` is correct as-is -- it's a flavor of
#         devel/php-composer (the old devel/php-composer2 v2-only port
#         was retired and merged back into devel/php-composer in 2023).
#       * `php85-pecl-redis` and `php85-pecl-uuid` are PECL-namespaced
#         ports (databases/pecl-redis, devel/pecl-uuid), NOT plain
#         `php85-redis`/`php85-uuid` -- using the wrong name here fails
#         at install time, not silently.
#       * `php85-maxminddb` is correct as-is (flavor of the
#         version-agnostic devel/php-maxminddb port).
#       * `php85-opcache` does NOT exist as a separate package anymore
#         -- it was removed from ports in Aug 2025 ("part of default
#         php85"). Opcache now ships bundled in the base `php85`
#         package; it's enabled via php.ini below instead of a pkg
#         install.
#
#   - `php85-fileinfo php85-iconv php85-simplexml php85-xmlreader
#     php85-xmlwriter php85-tokenizer`: confirmed missing during a real
#     install (2026-07) -- `composer install` on the actual application
#     fails without them (`Root composer.json requires PHP extension
#     ext-fileinfo/ext-iconv/ext-simplexml/ext-xmlreader/ext-xmlwriter *
#     but it is missing`, plus `nikic/php-parser` requiring
#     `ext-tokenizer`). These are compiled into PHP's Docker image by
#     default but aren't part of FreeBSD's base `php85` package, so the
#     Docker `install-php-extensions` list above isn't sufficient by
#     itself on FreeBSD. All six confirmed to exist as real packages via
#     `pkg search`.
#
#   - `audiowaveform` (util/docker/web/setup/audiowaveform.sh, BBC's
#     waveform-generation CLI used for track waveform previews) has NO
#     FreeBSD port as of this writing. It is NOT installed by this
#     script. If waveform previews are needed, build it manually from
#     https://codeberg.org/chrisn/audiowaveform (upstream moved off
#     GitHub) against `audio/libid3tag` and `devel/boost-libs` (FreeBSD
#     equivalents of the Debian libid3tag0/libboost-*1.83.0 deps in the
#     Docker script). This is flagged as an open item, not silently
#     dropped.
#
#   - The MaxMind DB-IP GeoIP database itself (util/docker/web/setup/dbip.sh)
#     is a *data* download, not a package, and isn't handled by any
#     script here. See freebsd/webapp/README.md for the manual/cron
#     follow-up.
#
#   - `node22`/`npm-node22`: matches `.github/workflows/default.yml`'s
#     `node-version: "lts/jod"` (Node 22 LTS, codename "Jod"). Verified
#     against FreshPorts (2026-07): `www/node22` is real (22.23.1) but does
#     NOT bundle npm -- its own pkg-message says to install `www/npm-node22`
#     separately, so both are listed below.

set -e

# --- Core web stack packages -------------------------------------------------
pkg install -y \
    nginx \
    php85 \
    php85-composer \
    php85-gd \
    php85-curl \
    php85-xml \
    php85-zip \
    php85-gmp \
    php85-pdo_mysql \
    php85-mbstring \
    php85-intl \
    php85-pecl-redis \
    php85-pecl-uuid \
    php85-ffi \
    php85-sockets \
    php85-maxminddb \
    php85-fileinfo \
    php85-iconv \
    php85-simplexml \
    php85-xmlreader \
    php85-xmlwriter \
    php85-tokenizer \
    valkey \
    ffmpeg \
    git \
    sudo \
    openssl \
    zstd \
    tmpreaper \
    node22 \
    npm-node22

# Enable FFI at the php.ini level (mirrors php.sh's
# `echo 'ffi.enable="true"' >> /usr/local/etc/php/conf.d/ffi.ini`).
# Used at runtime for StereoTool inspection.
mkdir -p /usr/local/etc/php
echo 'ffi.enable="true"' >> /usr/local/etc/php/conf.d/ffi.ini

# Enable opcache -- it's compiled into base php85 (no separate package
# to install, see the note above) but still needs turning on explicitly.
echo 'opcache.enable=1' >> /usr/local/etc/php/conf.d/opcache.ini
echo 'opcache.enable_cli=0' >> /usr/local/etc/php/conf.d/opcache.ini

# --- System user (mirrors util/docker/common/add_user.sh) -------------------
# The Docker image runs the app as a dedicated unprivileged `azuracast`
# user, home /var/azuracast, and adds it to the `www` group so it can
# share the php-fpm/nginx socket group perms (Debian used `www-data`;
# FreeBSD's nginx/php85-fpm packages both default to group `www`).
if ! pw usershow azuracast >/dev/null 2>&1; then
    pw useradd azuracast \
        -d /var/azuracast \
        -s /usr/sbin/nologin \
        -m \
        -c "AzuraCast"
fi
pw groupmod www -m azuracast

# --- Directory layout (mirrors add_user.sh + startup_scripts/03_persist_dir.sh) ---
mkdir -p /var/azuracast/www /var/azuracast/stations /var/azuracast/www_tmp \
    /var/azuracast/docs \
    /var/azuracast/backups \
    /var/azuracast/dbip \
    /var/azuracast/centrifugo \
    /var/azuracast/sftpgo/persist \
    /var/azuracast/sftpgo/backups \
    /var/azuracast/sftpgo/env.d \
    /var/azuracast/storage/uploads \
    /var/azuracast/storage/stereo_tool \
    /var/azuracast/storage/geoip \
    /var/azuracast/storage/sftpgo \
    /var/azuracast/storage/acme \
    /var/azuracast/www_tmp/nginx_client \
    /var/azuracast/www_tmp/nginx_fastcgi \
    /var/azuracast/www_tmp/nginx_cache \
    /var/azuracast/www_tmp/nginx_proxy \
    /var/azuracast/www_tmp/nginx_uwsgi \
    /var/azuracast/www_tmp/nginx_scgi \
    /var/azuracast/www_tmp/sftpgo_temp \
    /var/log/azuracast

chown -R azuracast:azuracast /var/azuracast /var/log/azuracast
chmod -R 777 /var/azuracast/www_tmp

# nginx's *compiled-in default* error log path (used for very early
# startup/argument-parsing messages before -c nginx.conf's own
# `error_log` directive has even been read) is /var/log/nginx/error.log,
# root-owned by default -- separate from the azuracast-owned
# /var/log/azuracast/nginx-error.log the custom nginx.conf sets for
# everything after config parsing succeeds. Confirmed during a real
# install: without this, nginx fails to start at all
# ("could not open error log file ... Permission denied") before it even
# gets to parsing the config that would redirect logging elsewhere.
mkdir -p /var/log/nginx
chown -R azuracast:azuracast /var/log/nginx

# php-fpm's default `pid = run/php-fpm.pid` resolves under
# /usr/local/etc/'s prefix convention to /var/run/php-fpm.pid --
# root-owned. supervisord launches php-fpm as root (see
# freebsd/webapp/supervisord.conf's `[supervisord] user = root`), but
# php-fpm's own pool config sets `user = azuracast`, so the master
# process privilege-drops to azuracast internally -- it does not stay
# root the way supervisord itself does. Confirmed during a real install:
# without this, php-fpm fails to start ("Unable to create the PID file
# ... Permission denied"). One-line sed
# patch to the package-owned config file, same pattern as the nginx
# mime.types M3U8 fix documented in freebsd/webapp/README.md.
if [ -f /usr/local/etc/php-fpm.conf ]; then
    sed -i '' 's|^pid = .*|pid = /var/azuracast/php-fpm.pid|' /usr/local/etc/php-fpm.conf
fi

# nginx's `ssl_certificate`/`ssl_certificate_key` (nginx.conf) point
# unconditionally at /var/azuracast/storage/acme/ssl.{crt,key}, which
# don't exist until a real domain's ACME certificate has actually been
# issued -- out of scope for initial platform bring-up (needs DNS/domain
# setup first). Confirmed during a real install: a fresh install has no
# cert at all, so nginx can't start even to serve a "not configured yet"
# page. Generate a throwaway self-signed placeholder so nginx can start;
# replace it with a real ACME-issued cert once one exists (this does NOT
# overwrite an existing cert, so it's safe to re-run this whole script).
if [ ! -f /var/azuracast/storage/acme/ssl.crt ]; then
    openssl req -x509 -nodes -days 365 -newkey rsa:2048 \
        -keyout /var/azuracast/storage/acme/ssl.key \
        -out /var/azuracast/storage/acme/ssl.crt \
        -subj "/CN=localhost"
    chown -R azuracast:azuracast /var/azuracast/storage/acme
fi
