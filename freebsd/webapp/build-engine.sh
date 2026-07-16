#!/bin/sh
#
# freebsd/webapp/build-engine.sh
#
# Builds the Rust streaming engine (engine/) from the deployed app checkout
# and installs the resulting binary to /usr/local/bin/azuracast-engine --
# the exact path App\Radio\Backend\StreamEngine::getBinary() hardcodes.
#
# Unlike 00-packages.sh/10-centrifugo.sh/11-sftpgo.sh/20-supervisor.sh (OS
# platform bootstrap, run once before the app is even checked out), this is
# an *app-deployment-time* step -- same category as configure-db.sh -- since
# it needs the actual source tree (specifically engine/) present first. Run
# it as part of step 4 in the top-level freebsd/README.md bring-up order
# ("Deploy the AzuraCast PHP application itself"), and again after any
# `git pull` that touches engine/.
#
# Source-of-truth notes (read before running):
#
#   - Dependency audit (2026-07, from engine/Cargo.toml): every dependency is
#     pure Rust with no system library requirement -- `symphonia`, `rubato`,
#     and `hound` are pure-Rust; `ebur128` is a pure-Rust *port* of the C
#     libebur128 library, not an FFI binding to it (confirmed via lib.rs/
#     docs.rs -- its optional `cc`/`cargo-c` build dependency is only for an
#     opt-in "build a C-compatible shared library" feature this project does
#     not enable). `reqwest` is already configured with
#     `default-features = false, features = ["rustls-tls"]` specifically so
#     it never needs a system OpenSSL on FreeBSD (see the comment in
#     Cargo.toml itself).
#   - **Not fully verified**: `rustls`'s crypto backend (transitively `ring`
#     or `aws-lc-rs`) has its own native build step that typically needs a C
#     compiler for a handful of assembly/C source files -- FreeBSD ships
#     `clang` in the base system since 10.0, which should satisfy this
#     without installing anything extra, but this has NOT been confirmed
#     against a real FreeBSD build as of this writing. If `cargo build`
#     fails with a `cc`/linker-not-found error from a `-sys` crate's build
#     script, `pkg install llvm` (or pin `CC`/`CXX` env vars to a specific
#     `/usr/local/bin/clangNN`) is the likely fix -- flagged here rather
#     than silently assumed to work.
#   - `pkg install rust` (lang/rust) is used rather than rustup, matching
#     this project's "use pkg for everything" convention already
#     established in 00-packages.sh. This intentionally leaves the full
#     Rust toolchain installed permanently on the jail (not a discardable
#     build stage like Docker's multi-stage builds) -- future `git pull`s
#     that touch engine/ need to re-run this script, so cargo/rustc staying
#     installed (with a warm `target/` incremental-build cache) is the
#     right tradeoff here, not a one-shot build-then-remove step.
#   - `AZURACAST_PATH` must already be set to wherever the app is checked
#     out (same env var convention as the crontab/rc.d script), default
#     `/var/azuracast/www` if unset (the canonical deploy path -- see
#     INSTALL.md step 5 for why it is not a free choice).

set -e

AZURACAST_PATH="${AZURACAST_PATH:-/var/azuracast/www}"
ENGINE_DIR="${AZURACAST_PATH}/engine"

if [ ! -d "$ENGINE_DIR" ]; then
    echo "error: $ENGINE_DIR not found -- is AZURACAST_PATH set correctly, and has the app been checked out?" >&2
    exit 1
fi

# --- Rust toolchain -----------------------------------------------------
if ! command -v cargo >/dev/null 2>&1; then
    pkg install -y rust
fi

# --- Build (release profile) ---------------------------------------------
cd "$ENGINE_DIR"
cargo build --release

# --- Install ---------------------------------------------------------------
install -m 755 target/release/azuracast-engine /usr/local/bin/azuracast-engine

echo "Installed /usr/local/bin/azuracast-engine:"
/usr/local/bin/azuracast-engine --version
