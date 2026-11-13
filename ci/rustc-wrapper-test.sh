#!/bin/sh
# Tests ci/rustc-wrapper.sh with fake rustc, kache and sccache (dot-4uzgol.111.1). No cargo, no network.
# Usage: sh ci/rustc-wrapper-test.sh   (ci/linux-nix.sh runs it before the build)
set -u
wrapper="$(cd "$(dirname "$0")" && pwd)/rustc-wrapper.sh"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
mkdir "$tmp/none" "$tmp/k" "$tmp/kbad" "$tmp/s" "$tmp/sbad"
fails=0

# A fake rustc: prints its args, exits with $FAKE_RC.
cat > "$tmp/rustc" <<'X'
#!/bin/sh
echo "rustc $*"
exit "${FAKE_RC:-0}"
X
# A fake cache tool: answers its probe, else reports itself and KACHE_LOCAL_ONLY, then runs "$@".
fake_tool() { # <path> <name> <probe rc>
  cat > "$1" <<X
#!/bin/sh
case "\$1" in --version|--start-server) exit $3 ;; esac
echo "$2 local=\${KACHE_LOCAL_ONLY:-unset}"
exec "\$@"
X
}
fake_tool "$tmp/k/kache" kache 0
fake_tool "$tmp/kbad/kache" kache 1
fake_tool "$tmp/s/sccache" sccache 0
fake_tool "$tmp/sbad/sccache" sccache 1
fake_tool "$tmp/kbin" kache-bin 0
chmod +x "$tmp/rustc" "$tmp"/*/* "$tmp/kbin"

# check <name> <expected stdout> <expected rc> <env assignments...>
check() {
  name=$1 want=$2 want_rc=$3
  shift 3
  got=$(env -i "$@" "$wrapper" "$tmp/rustc" --crate-name demo --edition 2021 2>&1)
  rc=$?
  if [ "$got" = "$want" ] && [ "$rc" = "$want_rc" ]; then
    echo "ok   $name"
  else
    echo "FAIL $name: rc=$rc (want $want_rc), output:"
    printf '%s\n' "$got" | sed 's/^/    /'
    echo "  want:"
    printf '%s\n' "$want" | sed 's/^/    /'
    fails=$((fails + 1))
  fi
}

plain="rustc --crate-name demo --edition 2021"
check "no cache tool on PATH: plain rustc" "$plain" 0 PATH="$tmp/none"
check "no tool, RUST_CACHE=sccache: plain rustc" "$plain" 0 PATH="$tmp/none" RUST_CACHE=sccache
check "rustc's exit code passes through" "$plain" 3 PATH="$tmp/none" FAKE_RC=3
check "kache on PATH, local only by default" "kache local=1
$plain" 0 PATH="$tmp/k"
check "KACHE_LOCAL_ONLY set is kept" "kache local=0
$plain" 0 PATH="$tmp/k" KACHE_LOCAL_ONLY=0
check "rustc's exit code passes through kache" "kache local=1
$plain" 4 PATH="$tmp/k" FAKE_RC=4
check "KACHE_BIN wins over PATH" "kache-bin local=1
$plain" 0 PATH="$tmp/k" KACHE_BIN="$tmp/kbin"
check "KACHE_BIN not executable: plain rustc" "$plain" 0 PATH="$tmp/k" KACHE_BIN="$tmp/missing"
check "kache that cannot start: plain rustc" "$plain" 0 PATH="$tmp/kbad"
check "RUST_CACHE=none ignores kache" "$plain" 0 PATH="$tmp/k" RUST_CACHE=none
check "unknown RUST_CACHE: plain rustc" "$plain" 0 PATH="$tmp/k" RUST_CACHE=ccache
check "RUST_CACHE=sccache uses sccache" "sccache local=unset
$plain" 0 PATH="$tmp/s:$tmp/k" RUST_CACHE=sccache
check "sccache that cannot start: plain rustc" "$plain" 0 PATH="$tmp/sbad" RUST_CACHE=sccache

got=$(env -i PATH="$tmp/none" "$wrapper" 2>&1)
if [ $? = 2 ]; then echo "ok   no arguments: usage, rc 2"; else echo "FAIL no arguments: $got"; fails=$((fails + 1)); fi

if [ "$fails" -ne 0 ]; then
  echo "rustc-wrapper-test: $fails failed"
  exit 1
fi
echo "rustc-wrapper-test: all passed"
