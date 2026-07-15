#!/bin/sh
#
# freebsd/webapp/11-sftpgo.sh
#
# Builds and installs SFTPGo (SFTP access for station media uploads)
# from source, since AzuraCast's Docker build installs a Debian .deb
# release asset that has no FreeBSD equivalent package.
#
# Source-of-truth version:
#   util/docker/web/setup/sftpgo.sh pins:
#     export SFTPGO_VERSION=2.6.4
#   and downloads sftpgo_2.6.4-1_<arch>.deb from the drakkan/sftpgo
#   GitHub releases. We build the matching v2.6.4 tag from source.
#
# Confirmed during a real install (2026-07): `go install
# github.com/drakkan/sftpgo/v2@<version>` (the pattern 10-centrifugo.sh
# uses successfully for Centrifugo) does NOT work for SFTPGo specifically
# -- it fails with:
#   "The go.mod file for the module providing named packages contains
#    one or more replace directives. It must not contain directives that
#    would cause it to be interpreted differently than if it were the
#    main module."
# This is a general Go toolchain restriction (`go install pkg@version`
# refuses to honor `replace` directives in a module's own go.mod), not a
# FreeBSD-specific problem -- it would fail identically on any OS.
# SFTPGo's go.mod has replace directives; Centrifugo's doesn't, which is
# why that script doesn't hit this. Cloning the tag and running a plain
# `go build` inside the checkout sidesteps the restriction entirely
# (replace directives are honored normally for a build run from within
# the module that declares them).
#
# Also confirmed during that same install: a plain `go build` only
# produces the binary -- it does NOT bundle the `templates/`/`static/`
# directories a normal release tarball ships alongside it. Without them,
# SFTPGo crashes on every startup attempt ("error loading required
# template: open .../templates/email/reset-password.html: no such file
# or directory"), fatal, permanent supervisord restart loop. So this
# script now also copies those directories out of the same clone before
# discarding it -- see the "unused" note this file used to carry about
# the built-in web UI being disabled doesn't apply here: the *email/
# notification* templates are read regardless of whether the web UI
# itself is enabled.

set -e

SFTPGO_VERSION="v2.6.4"
BUILD_DIR="/tmp/sftpgo-build"

pkg install -y go git

rm -rf "$BUILD_DIR"
git clone --depth 1 --branch "$SFTPGO_VERSION" https://github.com/drakkan/sftpgo.git "$BUILD_DIR"

(cd "$BUILD_DIR" && go build -ldflags "-s -w" -o /usr/local/bin/sftpgo)
chmod 0755 /usr/local/bin/sftpgo

mkdir -p /var/azuracast/sftpgo/persist \
    /var/azuracast/sftpgo/backups \
    /var/azuracast/sftpgo/env.d \
    /var/azuracast/sftpgo/templates \
    /usr/share/sftpgo

cp -R "$BUILD_DIR/templates/." /var/azuracast/sftpgo/templates/
cp -R "$BUILD_DIR/static" /usr/share/sftpgo/
cp -R "$BUILD_DIR/templates" /usr/share/sftpgo/

rm -rf "$BUILD_DIR"

touch /var/azuracast/sftpgo/sftpgo.db

chown -R azuracast:azuracast /var/azuracast/sftpgo

mkdir -p /var/azuracast/www_tmp/sftpgo_temp
chmod -R 777 /var/azuracast/www_tmp/sftpgo_temp

echo "Installed: $(/usr/local/bin/sftpgo version 2>&1 | head -n1)"
echo "Next: copy freebsd/webapp/sftpgo.json to"
echo "  /var/azuracast/sftpgo/sftpgo.json"
echo "and generate host keys (ssh-keygen -t rsa/-t ecdsa/-t ed25519) under"
echo "  /var/azuracast/storage/sftpgo/ (id_rsa, id_ecdsa, id_ed25519)"
echo "as util/docker/web/startup_scripts/07_sftpgo_conf.sh does in Docker."
