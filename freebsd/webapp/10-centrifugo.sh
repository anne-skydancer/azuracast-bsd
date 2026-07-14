#!/bin/sh
#
# freebsd/webapp/10-centrifugo.sh
#
# Builds and installs Centrifugo (websocket pub/sub used for the
# real-time Now Playing widget / live streaming updates) from source,
# since there is no FreeBSD package for it.
#
# Source-of-truth version:
#   util/docker/web/setup/centrifugo.sh pins:
#     export CENTRIFUGO_VERSION=6.9.0
#   and downloads the linux release tarball for v6.9.0. We build the
#   same v6.9.0 tag from source via `go install` instead, since that's
#   the only distribution channel available on FreeBSD (Centrifugo does
#   not publish FreeBSD binaries).
#
# Confirmed: pkg.go.dev lists `github.com/centrifugal/centrifugo/v6` as
# a `command` (main package at module root), so `go install
# github.com/centrifugal/centrifugo/v6@<version>` is a valid, supported
# way to build a specific tagged release.

set -e

CENTRIFUGO_VERSION="v6.9.0"

pkg install -y go

# GOBIN defaults to $(go env GOPATH)/bin, which for root is normally
# /root/go/bin. Pin it explicitly so the binary lands somewhere
# predictable regardless of who runs this script.
export GOBIN=/tmp/go-bin
mkdir -p "$GOBIN"

go install "github.com/centrifugal/centrifugo/v6@${CENTRIFUGO_VERSION}"

install -m 0755 "$GOBIN/centrifugo" /usr/local/bin/centrifugo

rm -rf "$GOBIN"

mkdir -p /var/azuracast/centrifugo
chown -R azuracast:azuracast /var/azuracast/centrifugo

echo "Installed: $(/usr/local/bin/centrifugo version 2>&1 | head -n1)"
echo "Next: copy freebsd/webapp/centrifugo-config.toml to"
echo "  /var/azuracast/centrifugo/config.toml"
