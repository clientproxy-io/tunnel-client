import hashlib
import json
import os
from pathlib import Path
import subprocess
import tarfile
import tempfile
import unittest


class SynologyPackageTest(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.binaries = self.root / "bin"
        self.binaries.mkdir()
        for arch in ("amd64", "arm64", "armv7"):
            (self.binaries / f"tunnel-client-linux-{arch}").write_text("#!/bin/sh\nexit 0\n")

    def build(self, version, override=None):
        output = self.root / version
        env = dict(os.environ)
        env.pop("SPK_BUILD", None)
        if override is not None:
            env["SPK_BUILD"] = override
        script = Path(__file__).with_name("build-spk.sh")
        result = subprocess.run(
            ["bash", str(script), version, str(self.binaries), str(output)],
            env=env, capture_output=True, text=True,
        )
        return result, output

    def test_release_build_increases_and_manifest_matches_package(self):
        versions = ("1.0.6", "1.1.0", "1.1.1", "1.1.2", "1.2.0", "2.0.0")
        previous_build = 1  # Published packages previously reused build 0001.
        for version in versions:
            with self.subTest(version=version):
                result, output = self.build(version)
                self.assertEqual(result.returncode, 0, result.stderr)
                manifest = json.loads((output / "synology-feed.json").read_text())
                build = int(manifest["version"].rsplit("-", 1)[1])
                self.assertGreater(build, previous_build)
                previous_build = build
                package = output / manifest["spk"]
                self.assertEqual(package.stat().st_size, manifest["size"])
                self.assertEqual(hashlib.md5(package.read_bytes()).hexdigest(), manifest["md5"])
                with tarfile.open(package) as archive:
                    info = archive.extractfile("INFO").read().decode()
                    self.assertIn(f'version="{manifest["version"]}"', info)
                    payload = archive.extractfile("package.tgz").read()
                    checksum = hashlib.md5(payload).hexdigest()
                    self.assertIn(f'checksum="{checksum}"', info)
        self.assertEqual(json.loads((self.root / "1.1.1" / "synology-feed.json").read_text())["version"], "1.1.1-1001001")

    def test_rebuild_can_increase_build(self):
        result, output = self.build("1.1.1", "1001002")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(json.loads((output / "synology-feed.json").read_text())["version"], "1.1.1-1001002")

    def test_invalid_versions_and_builds_are_rejected(self):
        for version, override in (("1.1.1", "0001"), ("1.1.1", "abc"),
                                  ("1.1.1", "2147483648"), ("1.1000.0", None),
                                  ("1.1.1-beta", None), ("2148.0.0", None)):
            with self.subTest(version=version, override=override):
                result, _ = self.build(version, override)
                self.assertNotEqual(result.returncode, 0)


if __name__ == "__main__":
    unittest.main()
