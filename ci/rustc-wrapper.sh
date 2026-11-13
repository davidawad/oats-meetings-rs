#!/bin/sh
# cargo's rustc-wrapper (.cargo/config.toml, dot-4uzgol.111.1): runs rustc through a build cache.
# cargo calls it as `ci/rustc-wrapper.sh <rustc> <args>...`.
#   RUST_CACHE=kache (default): kache ($KACHE_BIN, else on PATH), local cache only unless
#                               KACHE_LOCAL_ONLY is set.
#   RUST_CACHE=sccache:         sccache ($SCCACHE_BIN, else on PATH).
#   RUST_CACHE=none:            plain rustc.
# Fail open: an unknown RUST_CACHE, a missing tool or one that cannot start runs plain "$@", so
# the cache never fails a build. No lock, no jobs cap: cargo's own parallelism applies.
# POSIX sh on purpose: bare nix images and macOS have /bin/sh, not always bash 4.
if [ "$#" -eq 0 ]; then
  echo "rustc-wrapper.sh: usage: rustc-wrapper.sh <rustc> [args...]" >&2
  exit 2
fi

# The executable named by $1 (explicit path), else the first one on PATH, else nothing.
find_tool() {
  if [ -n "$1" ]; then
    [ -x "$1" ] && printf '%s\n' "$1"
    return 0
  fi
  command -v "$2" 2> /dev/null || true
}

case "${RUST_CACHE:-kache}" in
  kache)
    tool=$(find_tool "${KACHE_BIN:-}" kache)
    if [ -n "$tool" ] && "$tool" --version > /dev/null 2>&1; then
      KACHE_LOCAL_ONLY=${KACHE_LOCAL_ONLY:-1}
      export KACHE_LOCAL_ONLY
      exec "$tool" "$@"
    fi
    ;;
  sccache)
    tool=$(find_tool "${SCCACHE_BIN:-}" sccache)
    # --start-server is a no-op when the server runs, and fails when it cannot start.
    if [ -n "$tool" ] && "$tool" --start-server > /dev/null 2>&1; then
      exec "$tool" "$@"
    fi
    ;;
esac
exec "$@"
