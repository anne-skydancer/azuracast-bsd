#!/bin/sh
#
# freebsd/media-mirror-sync.sh
#
# HOST-side (not in-jail) one-way sync of the NAS music library to a
# local mirror directory, so playback never depends on the NAS being
# awake. The mirror -- not the NFS mount -- is what gets nullfs-mounted
# into the AzuraCast jail as its media storage; the NAS remains the only
# place you manage music (source of truth), and the mirror is fully
# disposable/rebuildable.
#
#   NAS (source of truth) --rsync, cron--> local mirror --nullfs--> jail
#
# Why: the reference deployment's NAS reboots far slower than the server
# after a power cut; with media read straight off NFS the station played
# its error jingle until the NAS deigned to appear (and the NFS `soft`
# mount was the standing suspect for occasional mid-track skips). With
# the mirror, a cold boot plays music from local disk immediately.
#
# Install (on the HOST):
#   1. pkg install -y rsync
#   2. cp <this file> /usr/local/sbin/azuracast-media-sync
#      chmod +x /usr/local/sbin/azuracast-media-sync
#      (copy from the jail's checkout, e.g.
#       <webapp-jail-path>/var/azuracast/www/freebsd/media-mirror-sync.sh)
#   3. Edit the three variables below (or override via environment).
#   4. Create the sentinel ONCE on the NAS share (while mounted):
#        touch /mnt/vault-music/.media-source-online
#      The sentinel is the mount guard -- see SAFETY below.
#   5. First sync by hand (long -- full library copy). ALWAYS launch
#      manual runs through the SAME lockf the cron line uses -- a
#      lockless manual run and a cron firing mid-copy means two rsyncs
#      fighting over the same target (learned live on the reference
#      install):
#        lockf -t 0 /var/run/azuracast-media-sync.lock /usr/local/sbin/azuracast-media-sync
#   6. Cron it (root's crontab on the host), e.g. every 20 minutes;
#      lockf(1) (base system) prevents overlapping runs, -t 0 makes an
#      already-running sync simply skip this cycle:
#        */20 * * * * lockf -t 0 /var/run/azuracast-media-sync.lock /usr/local/sbin/azuracast-media-sync >> /var/log/azuracast-media-sync.log 2>&1
#   7. Point the jail's media mount at the mirror: in the webapp jail's
#      stanza, nullfs-mount MIRROR_PATH onto whatever in-jail path the
#      station's Media storage location already uses (replacing any
#      direct NFS mount there), then `service jail restart <jail>`
#      (same path INSIDE the jail -- AzuraCast notices nothing).
#
# SAFETY -- the --delete trap this script exists to avoid: rsync with
# --delete from an UNMOUNTED NFS source directory syncs from "an empty
# directory" and dutifully deletes the entire mirror. The sentinel file
# lives on the NAS share itself, so it is only visible when the mount is
# genuinely present and serving; no sentinel -> no sync, try again next
# cron cycle. Do NOT create the sentinel on the local mountpoint
# directory while unmounted -- that would defeat the guard entirely.

set -eu

# --- configuration (edit here or override via environment) -----------------

# The NFS mountpoint on this host where the NAS library appears.
MEDIA_SOURCE="${MEDIA_SOURCE:-/mnt/vault-music}"

# Local mirror directory (needs the library's full size in free space).
MIRROR_PATH="${MIRROR_PATH:-/var/azuracast-media}"

# Sentinel file that must exist INSIDE the mounted share (see SAFETY).
SENTINEL="${SENTINEL:-.media-source-online}"

# ---------------------------------------------------------------------------

stamp() { date '+%Y-%m-%dT%H:%M:%S'; }

if [ ! -f "${MEDIA_SOURCE}/${SENTINEL}" ]; then
    echo "$(stamp) source sentinel ${MEDIA_SOURCE}/${SENTINEL} not present -- NAS not mounted/ready; skipping sync"
    exit 0
fi

mkdir -p "${MIRROR_PATH}"

echo "$(stamp) sync starting: ${MEDIA_SOURCE}/ -> ${MIRROR_PATH}/"

# -a           preserve structure/times (times matter: AzuraCast's media
#              scanner uses mtime to detect changed files)
# --delete     mirror deletions (guarded by the sentinel above)
# --partial    resume interrupted large files on the next run
# --exclude    keep the sentinel itself out of the library
# --chown      force the mirror to be owned by uid/gid 1001 (the
#              azuracast user, by this deployment's fixed convention --
#              numeric so it works whether this runs on the host or in a
#              jail, whatever the user is *named* there). Without it, -a
#              run as root preserves the NAS's ownership onto the mirror
#              and PHP gets "Failed to open directory: Permission
#              denied" on its own media dir (confirmed live) -- and any
#              manual chown gets silently reverted by the next sync.
rsync -a --delete --partial \
    --chown 1001:1001 \
    --exclude "/${SENTINEL}" \
    "${MEDIA_SOURCE}/" "${MIRROR_PATH}/"

echo "$(stamp) sync complete"
