#!/usr/bin/env python3
"""Fork-script tests for Machine (Agave 2.3.13 fork).

Covers the repo's own shell scripts and the vote-authority patcher —
NOT the bridge programs (those have their own suite in bridge/,
`cargo test --release`, 34/34) and NOT the live validator.

Safety rules (these tests must never break the chain):
- Never start a validator. Argument handling is tested with bad
  arguments (which exit before any start) and by executing ONLY the
  extracted argument-parsing block of start-validator.sh in a
  subshell — never the whole script with a valid argument.
- Never use --fresh anywhere. Never touch ledger/, ledger-test/ or
  keys/. The vote-authority patcher is tested ONLY against synthetic
  genesis files built in a temp dir, never against ledger/genesis.bin.
- The genesis-sanity tests are read-only (agave-ledger-tool
  genesis-hash / genesis, file reads, JSON parsing).

Run:  ./tests/run.sh        (or: python3 -m unittest tests.test_fork_scripts -v)
"""
import json
import os
import re
import struct
import subprocess
import sys
import tempfile
import unittest

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))

SHELL_SCRIPTS = [
    "start-validator.sh",
    "install.sh",
    "run-in-ns.sh",
    "livenet/start-validator.sh",
    "livenet/generate-genesis.sh",
    "livenet/generate-keys.sh",
    "livenet/check-cluster.sh",
    "ops/check-host.sh",
    "bridge/deploy/build-sbf.sh",
]

GENESIS_HASH = "32W8ouqrHbuhE3PM6ZsJrJcDbP7Tx9aC2zRvrEabkQSb"
FOUNDER = "7ivcPQJ6C7RZAnprKkJHN9o1PrpSjE6iC2mm7RRS3DEz"


def run(cmd, cwd=ROOT, **kw):
    return subprocess.run(cmd, cwd=cwd, capture_output=True, text=True, **kw)


def make_keypair(path, pubkey_bytes):
    """Write a 64-byte keypair JSON (32 secret bytes + 32 pubkey bytes)."""
    data = list(os.urandom(32)) + list(pubkey_bytes)
    with open(path, "w") as f:
        json.dump(data, f)


def make_synthetic_genesis(vote, founder, *, commission=100, voter=None,
                           node=None, withdrawer=None):
    """Build a minimal blob containing one vote account in the layout
    patch-vote-authority.py expects, so the patcher can be exercised
    without touching the real genesis."""
    if voter is None:
        voter = founder
    if node is None:
        node = founder
    if withdrawer is None:
        withdrawer = founder
    data = struct.pack("<I", 2)          # version
    data += node                          # node_pubkey
    data += withdrawer                    # authorized_withdrawer
    data += bytes([commission])           # commission
    data += struct.pack("<Q", 0)          # votes length
    data += bytes([0])                    # root_slot None
    data += struct.pack("<Q", 1)          # authorized_voters length
    data += struct.pack("<Q", 0)          # voter epoch
    data += voter                         # authorized voter
    # Real VoteState data is longer than the 126 bytes above (it carries
    # epoch credits, prior voters, etc.). Pad so data_len > 126, which
    # is what the patcher's locator requires.
    data += bytes(64)
    blob = b"FAKE-GENESIS-PREFIX" + vote
    blob += struct.pack("<Q", 1000)       # lamports
    blob += struct.pack("<Q", len(data))  # data length
    blob += data + b"FAKE-GENESIS-SUFFIX"
    return blob, len(b"FAKE-GENESIS-PREFIX") + 32 + 16


class TestShellSyntax(unittest.TestCase):
    def test_all_scripts_exist(self):
        for rel in SHELL_SCRIPTS:
            self.assertTrue(os.path.isfile(os.path.join(ROOT, rel)), rel)

    def test_bash_n_passes(self):
        """`bash -n` on every fork script — syntax only, nothing runs."""
        for rel in SHELL_SCRIPTS:
            r = run(["bash", "-n", rel])
            self.assertEqual(r.returncode, 0, f"bash -n failed for {rel}: {r.stderr}")

    def test_patch_script_compiles(self):
        r = run([sys.executable, "-m", "py_compile", "patch-vote-authority.py"])
        self.assertEqual(r.returncode, 0, r.stderr)


class TestStartValidatorArgs(unittest.TestCase):
    def test_unknown_argument_exits_1(self):
        r = run(["./start-validator.sh", "--bogus"])
        self.assertEqual(r.returncode, 1)
        self.assertIn("Unknown argument", r.stderr)

    def test_no_help_flag(self):
        """Documented behaviour: there is no --help; it is rejected
        like any other unknown argument."""
        r = run(["./start-validator.sh", "--help"])
        self.assertEqual(r.returncode, 1)
        self.assertIn("Unknown argument", r.stderr)

    def test_arg_parsing_block(self):
        """Execute ONLY the argument-parsing block extracted from
        start-validator.sh, with several argument lists. The validator
        is never started; the rest of the script is never executed."""
        src = open(os.path.join(ROOT, "start-validator.sh")).read()
        start = src.index("BACKGROUND=0")
        end = src.index('[ -x bin/solana-test-validator ]')
        block = src[start:end]
        self.assertIn('for arg in "$@"', block)
        for args, expected in [
            ([], "0/0"),
            (["--background"], "1/0"),
            (["--fresh"], "0/1"),
            (["--background", "--fresh"], "1/1"),
        ]:
            r = subprocess.run(
                ["bash", "-c", block + '\necho "$BACKGROUND/$FRESH"',
                 "arg-parse-test"] + args,
                capture_output=True, text=True)
            self.assertEqual(r.returncode, 0, r.stderr)
            self.assertEqual(r.stdout.strip(), expected, f"args={args}")
        r = subprocess.run(
            ["bash", "-c", block, "arg-parse-test", "--bogus"],
            capture_output=True, text=True)
        self.assertEqual(r.returncode, 1)
        self.assertIn("Unknown argument", r.stderr)


class TestLivenetScripts(unittest.TestCase):
    def test_livenet_start_bad_index_exits_1(self):
        r = run(["./livenet/start-validator.sh", "9"])
        self.assertEqual(r.returncode, 1)
        self.assertIn("Usage:", r.stderr)

    def test_livenet_start_no_index_exits_1(self):
        r = run(["./livenet/start-validator.sh"])
        self.assertEqual(r.returncode, 1)
        self.assertIn("Usage:", r.stderr)

    def test_livenet_start_missing_genesis_info(self):
        """Validator 1 passes the index check, then stops because
        livenet/genesis-info.env does not exist yet (no livenet
        genesis has been generated). Nothing starts."""
        self.assertFalse(os.path.exists(os.path.join(ROOT, "livenet/genesis-info.env")))
        r = run(["./livenet/start-validator.sh", "1"])
        self.assertEqual(r.returncode, 1)
        self.assertIn("genesis-info.env not found", r.stderr)

    def test_livenet_start_fd_guard_present(self):
        src = open(os.path.join(ROOT, "livenet/start-validator.sh")).read()
        self.assertIn("1000000", src)
        self.assertIn("ulimit -Hn", src)

    def test_generate_genesis_unknown_arg_exits_1(self):
        r = run(["./livenet/generate-genesis.sh", "--bogus"])
        self.assertEqual(r.returncode, 1)
        self.assertIn("Unknown argument", r.stderr)

    def test_generate_genesis_refuses_existing_ledger_without_force(self):
        """The script must refuse to overwrite an existing genesis
        unless --force is given. This test only checks the guard's
        presence in the source — it never runs the script with
        --force and never generates anything."""
        src = open(os.path.join(ROOT, "livenet/generate-genesis.sh")).read()
        self.assertIn("Refusing to overwrite an existing genesis", src)
        self.assertIn("--force", src)

    def test_validators_conf_ports_unique(self):
        conf = open(os.path.join(ROOT, "livenet/validators.conf")).read()
        gossip = re.findall(r"V\d_GOSSIP_PORT=(\d+)", conf)
        rpc = re.findall(r"V\d_RPC_PORT=(\d+)", conf)
        ranges = re.findall(r'V\d_DYNAMIC_RANGE="(\d+)-(\d+)"', conf)
        self.assertEqual(len(gossip), 3)
        self.assertEqual(len(set(gossip)), 3, "gossip ports must be distinct")
        self.assertEqual(len(rpc), 3)
        self.assertEqual(len(set(rpc)), 3, "RPC ports must be distinct")
        spans = [(int(a), int(b)) for a, b in ranges]
        self.assertEqual(len(spans), 3)
        for i, (a1, b1) in enumerate(spans):
            for a2, b2 in spans[i + 1:]:
                self.assertTrue(b1 < a2 or b2 < a1,
                                f"dynamic ranges overlap: {(a1, b1)} vs {(a2, b2)}")

    def test_livenet_distribution_sums_to_1b(self):
        """Parse the lamport amounts out of generate-genesis.sh and
        check they sum to exactly 1,000,000,000 MACHINE (1e18 lamports).
        The primordial split is five separate buckets (founder/team,
        ecosystem, community, validator rewards, treasury — see the
        header comment), plus 3 x 1,000,000 identity, 3 x 1,000,000
        stake, and a 1,000 faucet."""
        src = open(os.path.join(ROOT, "livenet/generate-genesis.sh")).read()
        balances = sorted(int(x) for x in re.findall(r"balance: (\d+)", src))
        self.assertEqual(len(balances), 5, f"expected 5 primordial buckets, got {balances}")
        self.assertEqual(balances, [150_000_000_000_000_000,
                                    150_000_000_000_000_000,
                                    200_000_000_000_000_000,
                                    243_999_000_000_000_000,
                                    250_000_000_000_000_000],
                         "primordial buckets do not match the documented split")
        m = re.search(r"--bootstrap-validator-lamports (\d+)", src)
        self.assertIsNotNone(m)
        per_validator = int(m.group(1))
        m = re.search(r"--bootstrap-validator-stake-lamports (\d+)", src)
        self.assertIsNotNone(m)
        per_stake = int(m.group(1))
        m = re.search(r"--faucet-lamports (\d+)", src)
        self.assertIsNotNone(m)
        faucet = int(m.group(1))
        total = sum(balances) + 3 * per_validator + 3 * per_stake + faucet
        self.assertEqual(total, 1_000_000_000 * 10**9,
                         f"livenet distribution sums to {total} lamports, not 1e18")


class TestPatchVoteAuthority(unittest.TestCase):
    def _setup_temp(self):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        keys = os.path.join(tmp.name, "keys")
        os.makedirs(keys)
        founder = os.urandom(32)
        vote = os.urandom(32)
        self.assertNotEqual(founder, vote)
        make_keypair(os.path.join(keys, "founder.json"), founder)
        make_keypair(os.path.join(keys, "vote.json"), vote)
        return tmp.name, founder, vote

    def _run_patch(self, cwd, genesis_path):
        return run([sys.executable,
                    os.path.join(ROOT, "patch-vote-authority.py"),
                    genesis_path], cwd=cwd)

    def test_patch_rewrites_voter_only(self):
        d, founder, vote = self._setup_temp()
        blob, data_start = make_synthetic_genesis(vote, founder)
        path = os.path.join(d, "genesis.bin")
        open(path, "wb").write(blob)
        r = self._run_patch(d, path)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("patched authorized voter", r.stdout)
        out = open(path, "rb").read()
        self.assertEqual(len(out), len(blob), "patch must not change file length")
        self.assertEqual(out[data_start + 94:data_start + 126], vote)
        self.assertEqual(out[data_start + 4:data_start + 36], founder,
                         "node_pubkey must be untouched")
        self.assertEqual(out[data_start + 36:data_start + 68], founder,
                         "authorized_withdrawer must be untouched")
        self.assertEqual(out[:data_start], blob[:data_start])
        self.assertEqual(out[data_start + 126:], blob[data_start + 126:])

    def test_patch_rejects_bad_commission_and_leaves_file_unchanged(self):
        d, founder, vote = self._setup_temp()
        blob, _ = make_synthetic_genesis(vote, founder, commission=0)
        path = os.path.join(d, "genesis.bin")
        open(path, "wb").write(blob)
        r = self._run_patch(d, path)
        self.assertEqual(r.returncode, 1)
        self.assertIn("layout check failed", r.stderr)
        self.assertEqual(open(path, "rb").read(), blob, "failed patch must not modify the file")

    def test_patch_rejects_already_patched(self):
        """A genesis whose voter is already the vote keypair fails the
        'current voter == founder' check and is left untouched."""
        d, founder, vote = self._setup_temp()
        blob, _ = make_synthetic_genesis(vote, founder, voter=vote)
        path = os.path.join(d, "genesis.bin")
        open(path, "wb").write(blob)
        r = self._run_patch(d, path)
        self.assertEqual(r.returncode, 1)
        self.assertEqual(open(path, "rb").read(), blob)

    def test_patch_rejects_missing_vote_account(self):
        d, founder, vote = self._setup_temp()
        path = os.path.join(d, "genesis.bin")
        open(path, "wb").write(os.urandom(4096))
        r = self._run_patch(d, path)
        self.assertEqual(r.returncode, 1)
        self.assertIn("could not locate the vote account", r.stderr)


class TestGenesisConfigSanity(unittest.TestCase):
    def test_genesis_bin_exists_and_hash_matches(self):
        path = os.path.join(ROOT, "ledger/genesis.bin")
        self.assertTrue(os.path.isfile(path), "shipped genesis missing")
        tool = os.path.join(ROOT, "bin/agave-ledger-tool")
        if not os.path.isfile(tool):
            self.skipTest("bin/agave-ledger-tool not installed; run ./install.sh")
        r = run([tool, "--ledger", "ledger", "genesis-hash"])
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn(GENESIS_HASH, r.stdout)

    def test_ledger_tool_genesis_params(self):
        tool = os.path.join(ROOT, "bin/agave-ledger-tool")
        if not os.path.isfile(tool):
            self.skipTest("bin/agave-ledger-tool not installed; run ./install.sh")
        r = run([tool, "--ledger", "ledger", "genesis"])
        self.assertEqual(r.returncode, 0, r.stderr)
        for needle in ["Shred version: 35836", "Ticks per slot: 64",
                       "Slots per epoch: 8192",
                       "Capitalization: 1000000000.2535667 SOL in 241 accounts",
                       "lamports_per_byte_year: 3480"]:
            self.assertIn(needle, r.stdout, needle)

    def test_genesis_md_matches_live_values(self):
        md = open(os.path.join(ROOT, "GENESIS.md")).read()
        for needle in [GENESIS_HASH, "35836", FOUNDER, "1,000,000,000",
                       "998,999,000", "10,000", "5,000", "100,000"]:
            self.assertIn(needle, md, needle)

    def test_dev_keypairs_are_64_byte_arrays(self):
        for name in ["founder", "vote", "stake", "faucet"]:
            data = json.load(open(os.path.join(ROOT, "keys", f"{name}.json")))
            self.assertEqual(len(data), 64, name)
            self.assertTrue(all(isinstance(x, int) and 0 <= x <= 255 for x in data), name)

    def test_install_pins_agave_2_3_13(self):
        src = open(os.path.join(ROOT, "install.sh")).read()
        self.assertIn('AGAVE_VERSION="2.3.13"', src)
        self.assertIn("5466f4592b1983adb13ba0a5d53f41ea2de69fba", src)
        tool = os.path.join(ROOT, "bin/solana")
        if os.path.isfile(tool):
            r = run([tool, "--version"])
            self.assertIn("2.3.13", r.stdout)


if __name__ == "__main__":
    unittest.main(verbosity=2)
