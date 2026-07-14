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
#   GitHub releases. We build the matching v2.6.4 tag from source via
#   `go install` instead.
#
# Confirmed: pkg.go.dev lists `github.com/drakkan/sftpgo/v2` as a
# `command` (main package at module root), so `go install
# github.com/drakkan/sftpgo/v2@<version>` is valid for a tagged release,
# same pattern as centrifugo in 10-centrifugo.sh.
#
# NOTE: the upstream SFTPGo Dockerfile's own release build passes extra
# ldflags (version stamping) and ships bundled httpd templates/static
# assets (/usr/share/sftpgo/{templates,static}). Those aren't produced
# by a plain `go install` and aren't needed here anyway — see the
# "unused" note on the `httpd` section in freebsd/webapp/sftpgo.json
# (the built-in web UI is disabled, so those paths are never read).

set -e

SFTPGO_VERSION="v2.6.4"

pkg install -y go

export GOBIN=/tmp/go-bin
mkdir -p "$GOBIN"

go install "github.com/drakkan/sftpgo/v2@${SFTPGO_VERSION}"

install -m 0755 "$GOBIN/sftpgo" /usr/local/bin/sftpgo

rm -rf "$GOBIN"

mkdir -p /var/azuracast/sftpgo/persist \
    /var/azuracast/sftpgo/backups \
    /var/azuracast/sftpgo/env.d

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
