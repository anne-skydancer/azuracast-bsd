#!/bin/sh
#
# freebsd/webapp/00-packages.sh
#
# Base package install + user/directory bootstrap for the `webapp` jail
# (10.8.0.110 / webapp.amc202d.lan). This jail runs nginx, php-fpm,
# Valkey, Centrifugo, SFTPGo, cron, and supervisord. It does NOT run
# MariaDB (separate `mariadb` jail, 10.8.0.100) or Icecast
# (separate `icecast` jail / out of scope here).
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
    valkey \
    ffmpeg \
    git \
    sudo \
    openssl \
    zstd \
    tmpreaper

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
    /var/azuracast/storage/shoutcast2 \
    /var/azuracast/storage/stereo_tool \
    /var/azuracast/storage/geoip \
    /var/azuracast/storage/sftpgo \
    /var/azuracast/storage/acme \
    /var/azuracast/www_tmp/nginx_client \
    /var/azuracast/www_tmp/nginx_fastcgi \
    /var/azuracast/www_tmp/nginx_cache \
    /var/azuracast/www_tmp/sftpgo_temp \
    /var/log/azuracast

chown -R azuracast:azuracast /var/azuracast /var/log/azuracast
chmod -R 777 /var/azuracast/www_tmp
