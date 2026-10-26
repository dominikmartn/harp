#!/usr/bin/env python3
"""Host orchestration regression tests; fake Docker never executes candidate code."""

import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


RUNNER = Path(__file__).with_name("test_release_runtime")
FAKE_DOCKER = r'''#!/usr/bin/env python3
import json, os, pathlib, signal, sys
root = pathlib.Path(os.environ['FAKE_ROOT'])
args = sys.argv[1:]
source = sys.stdin.read() if args[0] == 'exec' and '-i' in args else ''
with (root / 'calls.jsonl').open('a') as log:
    log.write(json.dumps({'args': args, 'source': source}) + '\n')
scenario = os.environ.get('FAKE_SCENARIO', 'success')
if args[0] == 'info':
    if scenario == 'unavailable': sys.exit(1)
    print('linux')
elif args[0] == 'context': print('unix:///var/run/docker.sock')
elif args[:2] == ['image', 'inspect']:
    print('ubuntu@sha256:' + 'a' * 64)
elif args[0] == 'create':
    counter = root / 'counter'
    number = int(counter.read_text()) + 1 if counter.exists() else 1
    counter.write_text(str(number))
    cid = format(number, '064x')
    pathlib.Path(args[args.index('--cidfile') + 1]).write_text(cid)
    print(cid)
elif args[0] == 'inspect':
    print('{"bridge":{}}' if scenario == 'network' else '{}')
elif args[0] == 'exec':
    if 'snapshot()' in source:
        if scenario == 'timeout': sys.exit(124)
        if scenario == 'failure': sys.exit(1)
        if scenario == 'capacity': sys.exit(41)
        if scenario == 'cancel':
            os.kill(int((root / 'runner.pid').read_text()), signal.SIGTERM)
    elif '-i' not in args:
        kind = args[-3]
        output = root / 'output' / kind
        output.mkdir(exist_ok=True)
        (output / 'baseline.txt').write_text('mesa-vulkan-drivers\nlibegl1\n')
        (output / 'launcher.json').write_text(json.dumps({
            'launcher_source': 'candidate-copy', 'launcher_sha256': 'b' * 64,
            'launcher_original_sha256': __import__('hashlib').sha256((root / 'Harp-x86_64.AppImage').read_bytes()).hexdigest(),
        }))
'''


class RuntimeGateTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="harp-runtime-tests-")
        self.root = Path(self.temporary.name)
        (self.root / "bin").mkdir()
        docker = self.root / "bin/docker"
        docker.write_text(FAKE_DOCKER)
        docker.chmod(0o755)
        python = self.root / "bin/python3"
        python.write_text(f'''#!{sys.executable}
import os, sys
if sys.argv[1:] == ['-', 'native-preflight']:
    sys.stdin.read()
    if os.environ.get('FAKE_SCENARIO') == 'binfmt':
        print('Unavailable: AppImage binfmt handler synthetic', file=sys.stderr)
        sys.exit(3)
    sys.exit(0)
os.execv({sys.executable!r}, [{sys.executable!r}, *sys.argv[1:]])
''')
        python.chmod(0o755)
        self.output = self.root / "output"
        self.output.mkdir()
        self.deb = self.root / "fixture.deb"
        self.appimage = self.root / "Harp-x86_64.AppImage"
        self.deb.write_bytes(b"mechanics-only deb fixture\n")
        self.appimage.write_bytes(b"mechanics-only appimage fixture\n")

    def tearDown(self):
        self.temporary.cleanup()

    def run_gate(self, scenario="success", mechanics="1", extra=(), compat="0"):
        env = dict(os.environ, FAKE_ROOT=str(self.root), FAKE_SCENARIO=scenario,
                   HARP_RUNTIME_MECHANICS_ONLY=mechanics, HARP_RUNTIME_BINFMT_COMPAT=compat,
                   HARP_RUNTIME_SOURCE_COMMIT="c" * 40)
        env["PATH"] = f"{self.root / 'bin'}:{os.environ['PATH']}"
        command = [str(RUNNER), "--deb", str(self.deb), "--appimage", str(self.appimage),
                   "--output-dir", str(self.output), *extra]
        process = subprocess.Popen(command, env=env, stdout=subprocess.PIPE,
                                   stderr=subprocess.PIPE, text=True)
        (self.root / "runner.pid").write_text(str(process.pid))
        stdout, stderr = process.communicate(timeout=20)
        return process.returncode, stdout, stderr

    def calls(self):
        log = self.root / "calls.jsonl"
        return [json.loads(line) for line in log.read_text().splitlines()] if log.exists() else []

    def test_success_manifest_and_isolation(self):
        status, stdout, stderr = self.run_gate()
        self.assertEqual(status, 0, stderr)
        self.assertEqual(stdout.strip(), str(self.output))
        result = json.loads((self.output / "result.json").read_text())
        self.assertIs(result["mechanics_only"], True)
        self.assertEqual(result["status"], "passed")
        self.assertEqual(result["source_commit"], "c" * 40)
        self.assertEqual(result["inputs"], {
            "deb": hashlib.sha256(self.deb.read_bytes()).hexdigest(),
            "appimage": hashlib.sha256(self.appimage.read_bytes()).hexdigest(),
        })
        creates = [c["args"] for c in self.calls() if c["args"][0] == "create"]
        self.assertEqual(len(creates), 2)
        for args in creates:
            self.assertEqual(args.count("--mount"), 2)
            self.assertIn("no-new-privileges", args)
            self.assertIn("--memory-swap", args)
            self.assertIn("/work:rw,exec,nosuid,nodev,size=3g,mode=1777", args)
            self.assertIn("readonly", " ".join(args))
            self.assertEqual(args[-5], result["image_digest"])
            self.assertNotIn("--privileged", args)
        calls = self.calls()
        for index, call in enumerate(calls):
            if "snapshot()" in call["source"]:
                self.assertEqual(calls[index - 1]["args"][0], "inspect")
                self.assertIn("--bounding-set=-all", call["source"])
                self.assertIn("env -i", call["source"])
                self.assertNotIn("bundled/metadata/version.json", call["source"])
        removals = [c["args"] for c in calls if c["args"][0] == "rm"]
        self.assertEqual([c[-1] for c in removals], [format(1, "064x"), format(2, "064x")])

    def test_candidate_classification_is_boolean(self):
        status, _, stderr = self.run_gate(mechanics="0")
        self.assertEqual(status, 0, stderr)
        self.assertIs(json.loads((self.output / "result.json").read_text())["mechanics_only"], False)
        self.assertEqual(json.loads((self.output / "result.json").read_text())["launch_mode"], "native-direct-exec")

    def test_compat_requires_mechanics_before_side_effects(self):
        status, _, _ = self.run_gate(mechanics="0", compat="1")
        self.assertEqual(status, 2)
        self.assertEqual(self.calls(), [])
        self.assertEqual(list(self.output.iterdir()), [])

    def test_native_binfmt_is_environment_failure(self):
        status, _, stderr = self.run_gate("binfmt")
        self.assertEqual(status, 3, stderr)
        self.assertEqual(self.calls(), [])
        self.assertIn("synthetic", stderr)

    def test_compat_cannot_be_release_evidence(self):
        status, _, stderr = self.run_gate(compat="1")
        self.assertEqual(status, 0, stderr)
        result = json.loads((self.output / "result.json").read_text())
        self.assertIs(result["mechanics_only"], True)
        self.assertEqual(result["launch_mode"], "external-runtime-binfmt-compat")
        self.assertEqual(result["launcher_source"], "candidate-copy")
        self.assertEqual(result["launcher_original_sha256"], result["inputs"]["appimage"])

    def test_failure_timeout_and_cancel_cleanup(self):
        for scenario, expected in (("failure", 1), ("timeout", 124), ("cancel", 143), ("capacity", 3)):
            with self.subTest(scenario=scenario):
                # Each scenario owns a separate output and fake Docker event log.
                for path in self.output.iterdir():
                    if path.is_dir():
                        for child in path.iterdir():
                            child.unlink()
                        path.rmdir()
                    else:
                        path.unlink()
                for filename in ("counter", "calls.jsonl"):
                    (self.root / filename).unlink(missing_ok=True)
                status, _, stderr = self.run_gate(scenario)
                self.assertEqual(status, expected, stderr)
                self.assertFalse((self.output / "result.json").exists())
                self.assertEqual((self.output / "exit-status").read_text().strip(), str(expected))
                removals = [c["args"] for c in self.calls() if c["args"][0] == "rm"]
                self.assertEqual(removals, [["rm", "--force", format(1, "064x")]])

    def test_network_must_be_disconnected(self):
        status, _, stderr = self.run_gate("network")
        self.assertEqual(status, 3, stderr)
        self.assertFalse(any("snapshot()" in c["source"] for c in self.calls()))
        self.assertTrue(any(c["args"][0] == "rm" for c in self.calls()))

    def test_unavailable_daemon(self):
        status, _, _ = self.run_gate("unavailable")
        self.assertEqual(status, 3)
        self.assertFalse(any(c["args"][0] == "create" for c in self.calls()))

    def test_nonempty_output_rejected_before_docker(self):
        (self.output / "owner-data").write_text("preserve me")
        status, _, _ = self.run_gate()
        self.assertEqual(status, 2)
        self.assertEqual(self.calls(), [])
        self.assertEqual((self.output / "owner-data").read_text(), "preserve me")

    def test_invalid_cli(self):
        status, _, _ = self.run_gate(extra=("--unknown",))
        self.assertEqual(status, 2)
        self.assertEqual(self.calls(), [])


if __name__ == "__main__":
    unittest.main()
