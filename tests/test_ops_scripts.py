#!/usr/bin/env python3
"""Behaviour tests for the ops / deploy / livenet scripts (scope 5).

Added by the 2026-11-19 code-review debate round. Unlike the string-
presence checks in test_fork_scripts.py, these tests EXECUTE the
scripts against stub binaries in temp directories and assert on the
verdicts the scripts produce. They never touch the repo's real
ledger/, keys/, livenet/keys/ or bin/, and never start a validator:
every external binary (solana, solana-keygen, solana-genesis,
agave-ledger-tool, agave-validator, cargo, curl) is a stub.

Run:  python3 -m unittest tests.test_ops_scripts -v
"""
import json
import os
import shutil
import subprocess
import sys
import tarfile
import tempfile
import threading
import unittest
from http.server import BaseHTTPRequestHandler, HTTPServer

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))


def run(cmd, cwd=None, env=None, **kw):
    e = dict(os.environ)
    if env:
        e.update(env)
    return subprocess.run(cmd, cwd=cwd, env=e, capture_output=True,
                          text=True, **kw)


def write(path, content, mode=0o644):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "w") as f:
        f.write(content)
    os.chmod(path, mode)


STUB_KEYGEN = """#!/bin/bash
# stub solana-keygen: `new ... -o FILE` writes a dummy; `pubkey FILE`
# prints the file's content (tests store the pubkey as the content).
if [ "$1" = "new" ]; then
    out=""
    prev=""
    for a in "$@"; do [ "$prev" = "-o" ] && out="$a"; prev="$a"; done
    echo "STUBKEY" > "$out"
    exit 0
fi
if [ "$1" = "pubkey" ]; then cat "$2"; exit 0; fi
if [ "$1" = "--version" ]; then echo "solana-keygen 2.3.13"; exit 0; fi
exit 0
"""


class TestCaps(unittest.TestCase):
    CAPS = os.path.join(ROOT, "bridge/deploy/caps.py")

    def caps(self, *args):
        return run([sys.executable, self.CAPS] + list(args))

    def test_normal_case_unchanged(self):
        r = self.caps("--planned-locked-supply", "10000000000")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("per_tx_cap            = 100000000", r.stdout)
        self.assertIn("daily_cap             = 500000000", r.stdout)

    def test_pct_above_100_rejected(self):
        r = self.caps("--planned-locked-supply", "1000000",
                      "--per-tx-pct", "150", "--daily-pct", "200")
        self.assertEqual(r.returncode, 1,
                         f"caps above total supply accepted: {r.stdout}")
        self.assertNotIn("Traceback", r.stderr)

    def test_nan_rejected_cleanly(self):
        r = self.caps("--planned-locked-supply", "1000000",
                      "--per-tx-pct", "nan")
        self.assertEqual(r.returncode, 1)
        self.assertNotIn("Traceback", r.stderr + r.stdout)

    def test_exact_integer_arithmetic(self):
        # Exact 5% of 1799999999999999999 is 89999999999999999.
        # float64 arithmetic rounds this UP to 90000000000000000.
        r = self.caps("--planned-locked-supply", "1799999999999999999",
                      "--per-tx-pct", "5", "--daily-pct", "5")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("per_tx_cap            = 89999999999999999", r.stdout)


class FakeRPC:
    """Minimal JSON-RPC stub serving canned Solana responses."""

    def __init__(self, vault_mint="SourceMint111", vault_is_token=True,
                 vault_amount="500", vault_decimals=6,
                 supply_amount="500", supply_decimals=6):
        self.vault_mint = vault_mint
        self.vault_is_token = vault_is_token
        self.vault_amount = vault_amount
        self.vault_decimals = vault_decimals
        self.supply_amount = supply_amount
        self.supply_decimals = supply_decimals
        outer = self

        class H(BaseHTTPRequestHandler):
            def log_message(self, *a):
                pass

            def do_POST(self):
                body = json.loads(self.rfile.read(
                    int(self.headers["Content-Length"])))
                method = body["method"]
                if method == "getTokenAccountBalance":
                    result = {"value": {"amount": outer.vault_amount,
                                        "decimals": outer.vault_decimals,
                                        "uiAmount": 0.0}}
                elif method == "getTokenSupply":
                    result = {"value": {"amount": outer.supply_amount,
                                        "decimals": outer.supply_decimals,
                                        "uiAmount": 0.0}}
                elif method == "getAccountInfo":
                    if outer.vault_is_token:
                        parsed = {"info": {
                            "mint": outer.vault_mint,
                            "owner": "VaultOwner111",
                            "tokenAmount": {
                                "amount": outer.vault_amount,
                                "decimals": outer.vault_decimals}},
                            "type": "account"}
                    else:
                        parsed = {"info": {}, "type": "account"}
                    result = {"value": {
                        "data": {"parsed": parsed,
                                 "program": "spl-token" if outer.vault_is_token
                                 else "system",
                                 "space": 165},
                        "executable": False, "lamports": 2039280,
                        "owner": "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA",
                        "rentEpoch": 0}}
                else:
                    result = {}
                out = json.dumps({"jsonrpc": "2.0", "id": 1,
                                  "result": result}).encode()
                self.send_response(200)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(out)))
                self.end_headers()
                self.wfile.write(out)

        self.server = HTTPServer(("127.0.0.1", 0), H)
        self.url = f"http://127.0.0.1:{self.server.server_address[1]}"
        self.thread = threading.Thread(target=self.server.serve_forever,
                                       daemon=True)
        self.thread.start()

    def stop(self):
        self.server.shutdown()


class TestReconcile(unittest.TestCase):
    SCRIPT = os.path.join(ROOT, "ops/reconcile.py")

    def reconcile(self, rpc):
        return run([sys.executable, self.SCRIPT,
                    "--solana-rpc", rpc.url, "--machine-rpc", rpc.url,
                    "--vault", "VaultAcct111", "--wrapped-mint", "Mint111"])

    def test_matching_equal_decimals_ok(self):
        rpc = FakeRPC()
        try:
            r = self.reconcile(rpc)
            self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
            self.assertIn("RECONCILE OK", r.stdout)
        finally:
            rpc.stop()

    def test_decimals_mismatch_fails(self):
        # Same raw amounts, different decimals: real values differ by
        # 10x, so a raw-amount comparison must NOT print OK.
        rpc = FakeRPC(supply_decimals=7)
        try:
            r = self.reconcile(rpc)
            self.assertEqual(r.returncode, 1,
                             f"decimals mismatch passed: {r.stdout}")
            self.assertIn("RECONCILE FAIL", r.stdout)
        finally:
            rpc.stop()

    def test_non_token_vault_fails(self):
        # The "vault" is not an SPL token account at all (e.g. an
        # operator pasted the wrong address): must not reconcile OK.
        rpc = FakeRPC(vault_is_token=False)
        try:
            r = self.reconcile(rpc)
            self.assertEqual(r.returncode, 1,
                             f"non-token vault passed: {r.stdout}")
        finally:
            rpc.stop()


CHECK_CLUSTER_STUB_SOLANA = """#!/bin/bash
# Stub `solana` CLI for check-cluster.sh. Behaviour via env:
#   STUB_TRANSFER_FAIL=1  -> the A->B transfer errors (no signature)
#   STUB_DELINQUENT=1     -> validator 3 listed as delinquent only
STATE="${STUB_STATE:?}"
cmd="$1"; shift
case "$cmd" in
  slot)
    n=$(cat "$STATE/slot" 2>/dev/null || echo 100); n=$((n+1))
    echo "$n" > "$STATE/slot"; echo "$n" ;;
  genesis-hash) echo "FakeGenesisHashXYZ" ;;
  validators)
    echo "Current Validators:"
    echo "  Identity                                      Vote Account"
    echo "  V1IDENTITY  V1VOTE"
    echo "  V2IDENTITY  V2VOTE"
    if [ "${STUB_DELINQUENT:-0}" != "1" ]; then
      echo "  V3IDENTITY  V3VOTE"
    fi
    echo "Delinquent Validators:"
    if [ "${STUB_DELINQUENT:-0}" = "1" ]; then
      echo "  V3IDENTITY  V3VOTE"
    fi
    ;;
  transfer)
    # Funding call ends up grepped for "Signature"; A->B is the one
    # whose keypair lives in a tmp dir.
    if [ "${STUB_TRANSFER_FAIL:-0}" = "1" ] && [ "$*" != "${*#v1-identity}" ]; then
      :
    fi
    if [ "${STUB_TRANSFER_FAIL:-0}" = "1" ]; then
      for a in "$@"; do
        case "$a" in *a.json*) echo "Error: insufficient funds" >&2; exit 1 ;;
        esac
      done
    fi
    echo "Signature: 5VERv8NMvzbJMEkV8xnrLkEaWRtSz9CosKDYjCJjBRnbJLgp8ziruBtd7SdbieP6SDJkfBdcCVfDPWTdNiKZP3Gd"
    ;;
  balance) echo "0.01 SOL" ;;
  *) echo "stub solana: unhandled $cmd" >&2; exit 1 ;;
esac
"""


class TestCheckCluster(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.mkdtemp()
        self.addCleanup(shutil.rmtree, self.tmp, True)
        live = os.path.join(self.tmp, "livenet")
        os.makedirs(live)
        for f in ["check-cluster.sh", "validators.conf"]:
            shutil.copy(os.path.join(ROOT, "livenet", f), live)
        write(os.path.join(live, "genesis-info.env"),
              "GENESIS_HASH=FakeGenesisHashXYZ\n"
              "V1_IDENTITY=V1IDENTITY\nV2_IDENTITY=V2IDENTITY\n"
              "V3_IDENTITY=V3IDENTITY\n")
        keys = os.path.join(live, "keys")
        os.makedirs(keys)
        write(os.path.join(keys, "v1-identity.json"), "V1IDENTITY")
        self.bindir = os.path.join(self.tmp, "bin")
        os.makedirs(self.bindir)
        write(os.path.join(self.bindir, "solana"),
              CHECK_CLUSTER_STUB_SOLANA, 0o755)
        write(os.path.join(self.bindir, "solana-keygen"), STUB_KEYGEN, 0o755)
        self.state = os.path.join(self.tmp, "state")
        os.makedirs(self.state)
        self.live = live

    def check(self, *args, **env):
        e = {"AGAVE_BIN": self.bindir, "STUB_STATE": self.state}
        e.update(env)
        return run(["./check-cluster.sh"] + list(args), cwd=self.live, env=e)

    def test_unknown_argument_rejected(self):
        r = self.check("--with-transfe")
        self.assertEqual(r.returncode, 1,
                         f"typo'd flag silently accepted: {r.stdout}")
        self.assertNotIn("LIVENET OK", r.stdout)

    def test_failed_transfer_is_not_ok(self):
        r = self.check("--with-transfer", STUB_TRANSFER_FAIL="1")
        self.assertNotEqual(r.returncode, 0,
                            f"failed transfer reported OK: {r.stdout}")
        self.assertNotIn("LIVENET OK", r.stdout)
        self.assertNotIn("transfer A -> B confirmed (sig ...,", r.stdout)

    def test_delinquent_validator_is_not_ok(self):
        r = self.check(STUB_DELINQUENT="1")
        self.assertNotEqual(r.returncode, 0,
                            f"delinquent validator reported OK: {r.stdout}")
        self.assertNotIn("LIVENET OK", r.stdout)

    def test_healthy_cluster_with_transfer_ok(self):
        r = self.check("--with-transfer")
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        self.assertIn("LIVENET OK", r.stdout)


class TestGenerateGenesisEmptyHash(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.mkdtemp()
        self.addCleanup(shutil.rmtree, self.tmp, True)
        live = os.path.join(self.tmp, "livenet")
        os.makedirs(live)
        for f in ["generate-genesis.sh", "generate-keys.sh"]:
            shutil.copy(os.path.join(ROOT, "livenet", f), live)
        keys = os.path.join(live, "keys")
        os.makedirs(keys)
        for name in ["v1-identity", "v1-vote", "v1-stake", "v2-identity",
                     "v2-vote", "v2-stake", "v3-identity", "v3-vote",
                     "v3-stake", "faucet", "treasury", "founder",
                     "ecosystem", "community", "validator-rewards"]:
            write(os.path.join(keys, name + ".json"), f"PK-{name}")
        self.bindir = os.path.join(self.tmp, "bin")
        os.makedirs(self.bindir)
        write(os.path.join(self.bindir, "solana-keygen"), STUB_KEYGEN, 0o755)
        write(os.path.join(self.bindir, "solana-genesis"), """#!/bin/bash
ledger=""
prev=""
for a in "$@"; do [ "$prev" = "--ledger" ] && ledger="$a"; prev="$a"; done
mkdir -p "$ledger"
touch "$ledger/genesis.bin" "$ledger/genesis.tar.bz2"
""", 0o755)
        self.live = live

    def ledger_tool(self, fail):
        body = "#!/bin/bash\nexit 1\n" if fail else """#!/bin/bash
for a in "$@"; do
  if [ "$a" = "genesis-hash" ]; then echo "FakeGenesisHashXYZ"; exit 0; fi
done
echo "Shred version: 4242"
"""
        write(os.path.join(self.bindir, "agave-ledger-tool"), body, 0o755)

    def test_failing_ledger_tool_must_not_write_empty_identity(self):
        self.ledger_tool(fail=True)
        r = run(["./generate-genesis.sh"], cwd=self.live,
                env={"AGAVE_BIN": self.bindir})
        self.assertNotEqual(r.returncode, 0,
                            f"empty genesis identity accepted: {r.stdout}")
        info = os.path.join(self.live, "genesis-info.env")
        if os.path.exists(info):
            content = open(info).read()
            self.assertNotIn("GENESIS_HASH=\n", content)
            self.assertNotIn("GENESIS_HASH=$", content)

    def test_happy_path_writes_identity(self):
        self.ledger_tool(fail=False)
        r = run(["./generate-genesis.sh"], cwd=self.live,
                env={"AGAVE_BIN": self.bindir})
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        content = open(os.path.join(self.live, "genesis-info.env")).read()
        self.assertIn("GENESIS_HASH=FakeGenesisHashXYZ", content)
        self.assertIn("SHRED_VERSION=4242", content)


class TestGenerateKeysForceGuard(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.mkdtemp()
        self.addCleanup(shutil.rmtree, self.tmp, True)
        live = os.path.join(self.tmp, "livenet")
        os.makedirs(live)
        shutil.copy(os.path.join(ROOT, "livenet/generate-keys.sh"), live)
        self.keys = os.path.join(live, "keys")
        os.makedirs(self.keys)
        write(os.path.join(self.keys, "treasury.json"), "ORIGINAL-KEY")
        self.bindir = os.path.join(self.tmp, "bin")
        os.makedirs(self.bindir)
        write(os.path.join(self.bindir, "solana-keygen"), STUB_KEYGEN, 0o755)
        self.live = live

    def test_force_refused_when_genesis_exists(self):
        write(os.path.join(self.live, "genesis-info.env"),
              "GENESIS_HASH=FakeGenesisHashXYZ\n")
        r = run(["./generate-keys.sh", "--force"], cwd=self.live,
                env={"AGAVE_BIN": self.bindir})
        self.assertEqual(r.returncode, 1,
                         f"--force destroyed keys after genesis: {r.stdout}")
        self.assertEqual(open(os.path.join(self.keys, "treasury.json")).read(),
                         "ORIGINAL-KEY")

    def test_force_allowed_before_genesis(self):
        r = run(["./generate-keys.sh", "--force"], cwd=self.live,
                env={"AGAVE_BIN": self.bindir})
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        self.assertNotEqual(
            open(os.path.join(self.keys, "treasury.json")).read(),
            "ORIGINAL-KEY")


class TestInstallVerification(unittest.TestCase):
    def test_unverified_tarball_refused(self):
        tmp = tempfile.mkdtemp()
        self.addCleanup(shutil.rmtree, tmp, True)
        shutil.copy(os.path.join(ROOT, "install.sh"), tmp)
        # Build a plausible-looking release tarball whose binaries are
        # stubs. Its hash is NOT the pinned Agave 2.3.13 hash.
        stage = os.path.join(tmp, "stage/solana-release/bin")
        os.makedirs(stage)
        for b in ["solana", "solana-keygen", "solana-genesis",
                  "solana-test-validator", "agave-validator",
                  "agave-ledger-tool"]:
            write(os.path.join(stage, b),
                  '#!/bin/bash\necho "solana-cli 2.3.13 (src:fake)"\n', 0o755)
        tarball = os.path.join(tmp, "fake.tar.bz2")
        with tarfile.open(tarball, "w:bz2") as tf:
            tf.add(os.path.join(tmp, "stage/solana-release"),
                   arcname="solana-release")
        stubbin = os.path.join(tmp, "stubbin")
        os.makedirs(stubbin)
        write(os.path.join(stubbin, "curl"), f"""#!/bin/bash
out=""
prev=""
for a in "$@"; do [ "$prev" = "-o" ] && out="$a"; prev="$a"; done
cp "{tarball}" "$out"
""", 0o755)
        r = run(["./install.sh"], cwd=tmp,
                env={"PATH": stubbin + ":" + os.environ["PATH"]})
        self.assertNotEqual(r.returncode, 0,
                            f"unverified tarball installed: {r.stdout}")
        self.assertFalse(os.path.exists(os.path.join(tmp, "bin/solana")),
                         "binaries from an unverified tarball were installed")
        self.assertIn("sha256", (r.stdout + r.stderr).lower())


class TestLivenetStartValidator(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.mkdtemp()
        self.addCleanup(shutil.rmtree, self.tmp, True)
        live = os.path.join(self.tmp, "livenet")
        os.makedirs(live)
        shutil.copy(os.path.join(ROOT, "livenet/start-validator.sh"), live)
        self.conf = os.path.join(live, "validators.conf")
        shutil.copy(os.path.join(ROOT, "livenet/validators.conf"), self.conf)
        write(os.path.join(live, "genesis-info.env"),
              "GENESIS_HASH=FakeGenesisHashXYZ\nSHRED_VERSION=4242\n"
              "V1_IDENTITY=V1IDENTITY\nV1_VOTE=V1VOTE\n"
              "V2_IDENTITY=V2IDENTITY\nV2_VOTE=V2VOTE\n"
              "V3_IDENTITY=V3IDENTITY\nV3_VOTE=V3VOTE\n")
        keys = os.path.join(live, "keys")
        os.makedirs(keys)
        for n in (1, 2, 3):
            write(os.path.join(keys, f"v{n}-identity.json"), f"V{n}IDENTITY")
            write(os.path.join(keys, f"v{n}-vote.json"), f"V{n}VOTE")
        led = os.path.join(live, "ledger")
        os.makedirs(led)
        write(os.path.join(led, "genesis.bin"), "genesis")
        self.bindir = os.path.join(self.tmp, "bin")
        os.makedirs(self.bindir)
        write(os.path.join(self.bindir, "solana-keygen"), STUB_KEYGEN, 0o755)
        write(os.path.join(self.bindir, "agave-validator"),
              "#!/bin/bash\necho STUB-VALIDATOR-STARTED\n", 0o755)
        self.live = live

    def start(self, n, env=None):
        e = {"AGAVE_BIN": self.bindir}
        if env:
            e.update(env)
        return run(["./start-validator.sh", str(n)], cwd=self.live, env=e)

    def test_entrypoint_uses_configured_gossip_port(self):
        conf = open(self.conf).read().replace("V1_GOSSIP_PORT=8001",
                                              "V1_GOSSIP_PORT=8123")
        conf = conf.replace('V1_PUBLIC_IP=""', 'V1_PUBLIC_IP="203.0.113.7"')
        conf = conf.replace('V2_PUBLIC_IP=""', 'V2_PUBLIC_IP="203.0.113.8"')
        write(self.conf, conf)
        r = self.start(2)
        self.assertIn("entrypoint: 203.0.113.7:8123", r.stdout,
                      f"entrypoint ignored configured port: {r.stdout}")

    def test_identity_mismatch_refused(self):
        write(os.path.join(self.live, "keys/v1-identity.json"), "WRONGKEY")
        conf = open(self.conf).read().replace(
            'V1_PUBLIC_IP=""', 'V1_PUBLIC_IP="203.0.113.7"')
        write(self.conf, conf)
        r = self.start(1)
        self.assertEqual(r.returncode, 1)
        self.assertIn("does not match", r.stdout + r.stderr,
                      f"mismatched identity accepted: {r.stdout}{r.stderr}")
        self.assertNotIn("STUB-VALIDATOR-STARTED", r.stdout)

    def test_invalid_auto_detected_ip_refused(self):
        stubbin = os.path.join(self.tmp, "stubbin")
        os.makedirs(stubbin)
        write(os.path.join(stubbin, "curl"),
              "#!/bin/bash\necho '<html>proxy error</html>'\n", 0o755)
        r = self.start(1, env={
            "PATH": stubbin + ":" + os.environ["PATH"]})
        self.assertEqual(r.returncode, 1)
        self.assertIn("not a valid IP", r.stdout + r.stderr,
                      f"garbage gossip host accepted: {r.stdout}{r.stderr}")


class TestRunInNs(unittest.TestCase):
    def test_garbage_sysctl_content_refused(self):
        tmp = tempfile.mkdtemp()
        self.addCleanup(shutil.rmtree, tmp, True)
        for k in ["rmem_max", "wmem_max", "rmem_default", "wmem_default"]:
            write(os.path.join(tmp, k), "not-a-number\n")
        r = run([os.path.join(ROOT, "run-in-ns.sh"), "true"],
                env={"SYSCTL_FAKE_DIR": tmp})
        self.assertEqual(r.returncode, 1,
                         f"garbage sysctl values accepted: {r.stdout}")
        self.assertIn("positive integer", r.stdout + r.stderr)

    def test_valid_sysctl_content_accepted(self):
        tmp = tempfile.mkdtemp()
        self.addCleanup(shutil.rmtree, tmp, True)
        for k in ["rmem_max", "wmem_max", "rmem_default", "wmem_default"]:
            write(os.path.join(tmp, k), "134217728\n")
        r = run([os.path.join(ROOT, "run-in-ns.sh"), "true"],
                env={"SYSCTL_FAKE_DIR": tmp})
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)


class TestRootStartValidatorSeeding(unittest.TestCase):
    def test_partial_working_ledger_is_reseeded(self):
        tmp = tempfile.mkdtemp()
        self.addCleanup(shutil.rmtree, tmp, True)
        shutil.copy(os.path.join(ROOT, "start-validator.sh"), tmp)
        os.makedirs(os.path.join(tmp, "ledger"))
        write(os.path.join(tmp, "ledger/genesis.bin"), "genesis")
        os.makedirs(os.path.join(tmp, "keys"))
        for k in ["founder", "vote", "stake", "faucet"]:
            write(os.path.join(tmp, "keys", f"{k}.json"), f"KEY-{k}")
        os.makedirs(os.path.join(tmp, "bin"))
        write(os.path.join(tmp, "bin/solana-test-validator"),
              "#!/bin/bash\necho STUB-TEST-VALIDATOR\n", 0o755)
        # Simulate an interrupted first run: ledger-test exists with
        # the genesis copied but the keypairs never copied.
        os.makedirs(os.path.join(tmp, "ledger-test"))
        write(os.path.join(tmp, "ledger-test/genesis.bin"), "genesis")
        r = run(["./start-validator.sh"], cwd=tmp)
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        for name in ["validator-keypair.json", "vote-account-keypair.json",
                     "stake-account-keypair.json", "faucet-keypair.json"]:
            self.assertTrue(
                os.path.exists(os.path.join(tmp, "ledger-test", name)),
                f"{name} missing after start with partial ledger-test")


STUB_CARGO = """#!/bin/bash
# Stub cargo: `build-sbf --help` succeeds; a build records its
# manifest path to $STUB_CARGO_LOG and produces the .so where a real
# cargo build-sbf would (workspace target/deploy).
if [ "$2" = "--help" ]; then echo "cargo-build-sbf stub"; exit 0; fi
manifest=""
prev=""
for a in "$@"; do [ "$prev" = "--manifest-path" ] && manifest="$a"; prev="$a"; done
echo "$manifest" >> "${STUB_CARGO_LOG:?}"
crate=$(basename "$(dirname "$manifest")")
ws=$(cd "$(dirname "$manifest")/../.." && pwd)
mkdir -p "$ws/target/deploy"
echo "stub-sbf-$crate" > "$ws/target/deploy/$(echo "$crate" | tr '-' '_').so"
"""


class TestBuildSbf(unittest.TestCase):
    def test_build_uses_scratch_tree_and_pinned_lockfile(self):
        tmp = tempfile.mkdtemp()
        self.addCleanup(shutil.rmtree, tmp, True)
        bridge = os.path.join(tmp, "bridge")
        deploy = os.path.join(bridge, "deploy")
        os.makedirs(deploy)
        shutil.copy(os.path.join(ROOT, "bridge/deploy/build-sbf.sh"), deploy)
        shutil.copy(os.path.join(ROOT, "bridge/deploy/sbf-Cargo.lock"), deploy)
        for crate in ["bridge-common", "solana-lock", "machine-bridge"]:
            src = os.path.join(ROOT, "bridge/crates", crate, "Cargo.toml")
            dst = os.path.join(bridge, "crates", crate)
            os.makedirs(dst)
            shutil.copy(src, dst)
        stubbin = os.path.join(tmp, "stubbin")
        os.makedirs(stubbin)
        write(os.path.join(stubbin, "cargo"), STUB_CARGO, 0o755)
        log = os.path.join(tmp, "cargo.log")
        r = run(["./build-sbf.sh"], cwd=deploy, env={
            "PATH": stubbin + ":" + os.environ["PATH"],
            "STUB_CARGO_LOG": log})
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        for so in ["solana_lock.so", "machine_bridge.so"]:
            self.assertTrue(os.path.exists(
                os.path.join(bridge, "target/deploy", so)), so)
        manifests = open(log).read()
        # Builds ran against the scratch copies, not the workspace.
        self.assertIn("sbf-scratch", manifests)
        self.assertNotIn(os.path.join(bridge, "crates/solana-lock"), manifests)
        scratch = os.path.join(bridge, "target/sbf-scratch")
        # The scratch lockfile is the pinned one, byte for byte.
        self.assertEqual(
            open(os.path.join(scratch, "Cargo.lock")).read(),
            open(os.path.join(ROOT, "bridge/deploy/sbf-Cargo.lock")).read())
        # Dev-dependencies were stripped from the scratch manifests.
        for crate in ["solana-lock", "machine-bridge"]:
            manifest = open(os.path.join(
                scratch, "crates", crate, "Cargo.toml")).read()
            self.assertNotIn("[dev-dependencies]", manifest)


class TestSystemdHardening(unittest.TestCase):
    def test_validator_units_have_basic_sandboxing(self):
        for n in (1, 2, 3):
            src = open(os.path.join(
                ROOT, "livenet", f"mememachine-validator-{n}.service")).read()
            for directive in ["NoNewPrivileges=true", "PrivateTmp=true",
                              "ProtectSystem=strict", "ReadWritePaths="]:
                self.assertIn(directive, src, f"validator {n}: {directive}")

    def test_reconcile_unit_has_basic_sandboxing(self):
        src = open(os.path.join(ROOT, "ops/mememachine-reconcile.service")).read()
        for directive in ["NoNewPrivileges=true", "PrivateTmp=true",
                          "ProtectSystem=strict"]:
            self.assertIn(directive, src, directive)


class TestUpShOrdering(unittest.TestCase):
    def test_validators_enabled_only_after_proof(self):
        src = open(os.path.join(ROOT, "testnet/up.sh")).read()
        proof = src.index("check-cluster.sh --with-transfer")
        enable = src.index("systemctl enable")
        self.assertLess(proof, enable,
                        "validators are boot-enabled before the proof step")
        self.assertNotIn("enable --now", src)


if __name__ == "__main__":
    unittest.main(verbosity=2)
