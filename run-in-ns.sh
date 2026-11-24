#!/bin/bash
# run-in-ns.sh — run a command inside a user+mount namespace where the
# OS network sysctls are bind-mounted to Agave's recommended values.
#
# Why: agave-validator refuses to start when net.core.rmem_max (etc.) are
# below its recommended values, and this container cannot change the real
# sysctls (no CAP_SYS_ADMIN on the host). Inside a user namespace we are
# root, so we can bind-mount files with the recommended values over the
# /proc/sys entries. The mounts exist only inside the namespace and vanish
# when the process exits; the host sysctls are never changed.
# This only fakes the CONFIG CHECK — consensus, voting and ledger behaviour
# are untouched (local testnet traffic fits in the real 4MB buffers).
set -e
[ $# -ge 1 ] || { echo "Usage: $0 <command> [args...]" >&2; exit 1; }
FAKE_DIR="${SYSCTL_FAKE_DIR:-/tmp/sysctl-fake}"
for k in rmem_max wmem_max rmem_default wmem_default; do
    [ -f "$FAKE_DIR/$k" ] || { echo "missing fake sysctl file: $FAKE_DIR/$k" >&2; exit 1; }
    # The default directory is a predictable path in a shared /tmp:
    # never bind-mount contents nobody checked. Each file must hold
    # exactly one positive integer (the sysctl value it stands for).
    v=$(cat "$FAKE_DIR/$k")
    case "$v" in
        ''|*[!0-9]*)
            echo "fake sysctl file $FAKE_DIR/$k does not contain a positive integer (got: '$v')" >&2
            exit 1 ;;
    esac
    [ "$v" -gt 0 ] || { echo "fake sysctl file $FAKE_DIR/$k does not contain a positive integer (got: '$v')" >&2; exit 1; }
done
exec unshare --user --mount --map-root-user bash -c '
set -e
mount --bind "$1/rmem_max" /proc/sys/net/core/rmem_max
mount --bind "$1/wmem_max" /proc/sys/net/core/wmem_max
mount --bind "$1/rmem_default" /proc/sys/net/core/rmem_default
mount --bind "$1/wmem_default" /proc/sys/net/core/wmem_default
shift
exec "$@"
' _ "$FAKE_DIR" "$@"
