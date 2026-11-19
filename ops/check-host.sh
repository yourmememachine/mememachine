#!/bin/bash
# check-host.sh — can this host run a Machine validator at all?
#
# The two hard blockers that stopped the old build VM (see
# LAUNCH_CHECKLIST.md items 17-18) are checked first and in plain
# language:
#   1. UDP send must work (sendto must not return EPERM) — gossip and
#      RPC -> TPU forwarding need it; without it user transfers and
#      faucet airdrops fail.
#   2. The open-file hard limit must be raisable to 1,000,000 —
#      agave-validator refuses to start below that.
#
# Usage: ./ops/check-host.sh
# Exit code 0 = host is suitable. Non-zero = it is not; the FAIL
# lines say exactly what to fix or why to pick another host.
set -u

fail=0
say() { printf '%-58s %s\n' "$1" "$2"; }

echo "Machine host readiness check"
echo

# 1. UDP send. (Needs python3; a missing interpreter must not be
# reported as a UDP failure — that misdiagnosis sends the operator
# hunting for a network problem that does not exist.)
if ! command -v python3 >/dev/null 2>&1; then
    say "UDP send works" "FAIL — python3 is not installed, so UDP send cannot be tested. Install python3 and re-run."
    fail=1
elif python3 - <<'EOF' 2>/dev/null
import socket, sys
s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
try:
    s.sendto(b"mememachine-host-check", ("8.8.8.8", 53))
except OSError as e:
    print(e, file=sys.stderr)
    sys.exit(1)
EOF
then
    say "UDP send works" "OK"
else
    say "UDP send works" "FAIL — sendto blocked (EPERM). This host/container cannot run a validator; move to a VPS/bare metal that allows UDP."
    fail=1
fi

# 2. File-descriptor limit.
HARD=$(ulimit -Hn)
if [ "$HARD" = "unlimited" ] || { [ -n "$HARD" ] && [ "$HARD" -ge 1000000 ] 2>/dev/null; }; then
    say "open-file hard limit ($HARD)" "OK"
else
    say "open-file hard limit ($HARD, need 1000000)" "FAIL — raise it for the validator user (livenet/DEPLOY.md step 2) and re-login/reboot; if the hard limit cannot move, this host cannot run agave-validator."
    fail=1
fi

# 3. Basics: OS, CPU, RAM, disk.
if [ -r /etc/os-release ]; then
    # shellcheck disable=SC1091
    . /etc/os-release
    say "OS (${PRETTY_NAME:-unknown})" "INFO"
fi
CPUS=$(nproc 2>/dev/null || echo "?")
if [ "$CPUS" != "?" ] && [ "$CPUS" -ge 2 ]; then
    say "CPUs ($CPUS, want 2+)" "OK"
else
    say "CPUs ($CPUS, want 2+)" "FAIL"
    fail=1
fi
RAM_MB=$(awk '/MemTotal/ {print int($2/1024)}' /proc/meminfo 2>/dev/null || echo 0)
if [ "$RAM_MB" -ge 4096 ]; then
    say "RAM (${RAM_MB} MB, want 4096+)" "OK"
else
    say "RAM (${RAM_MB} MB, want 4096+)" "FAIL"
    fail=1
fi
# Measure the filesystem the ledger will actually live on (the repo
# this script belongs to), not wherever the operator happens to stand.
REPO_DIR="$(cd "$(dirname "$0")/.." && pwd)"
DISK_GB=$(df -Pk "$REPO_DIR" | awk 'NR==2 {print int($4/1024/1024)}')
if [ "$DISK_GB" -ge 50 ]; then
    say "free disk on repo filesystem (${DISK_GB} GB, want 50+)" "OK"
else
    say "free disk on repo filesystem (${DISK_GB} GB, want 50+)" "FAIL"
    fail=1
fi

# 4. Tools the deploy needs.
for tool in git curl python3; do
    if command -v "$tool" >/dev/null 2>&1; then
        say "tool: $tool" "OK"
    else
        say "tool: $tool" "FAIL — install it first"
        fail=1
    fi
done

echo
if [ "$fail" = 0 ]; then
    echo "HOST OK — this host can run Machine validators."
    echo "Next: livenet/DEPLOY.md (keys, genesis, start, check-cluster.sh --with-transfer)."
else
    echo "HOST NOT SUITABLE — fix the FAIL lines above, or move to a host that passes."
    exit 1
fi
