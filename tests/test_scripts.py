"""Shell behavior tests use only fixture downloads and private temporary installs."""
import hashlib
import io
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tarfile
import tempfile
import time
import unittest

ROOT = Path(__file__).resolve().parents[1]


class ScriptTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory(prefix="ht-shell-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.bin = self.root / "bin"
        self.bin.mkdir()
        self.downloads = self.root / "downloads"
        self.downloads.mkdir()
        self.env = {**os.environ, "PATH": f"{self.bin}:{os.environ['PATH']}", "FIXTURE_DOWNLOADS": str(self.downloads), "FIXTURE_PLATFORM": "Linux-x86_64"}
        (self.root / "herdr-plugin.toml").write_text('version = "0.2.0"\n')
        self.installed = self.root / "target/release/herdr-tokens"
        self.installed.parent.mkdir(parents=True)
        self.executable(self.installed, "#!/bin/sh\necho previous\n")
        self.executable(self.bin / "uname", '#!/bin/sh\ncase "$1" in -s) echo "${FIXTURE_PLATFORM%-*}";; -m) echo "${FIXTURE_PLATFORM##*-}";; esac\n')
        self.executable(self.bin / "curl", f'''#!{sys.executable}
import os, pathlib, shutil, sys
url = next(a for a in sys.argv if a.startswith('https://'))
destination = sys.argv[sys.argv.index('-o') + 1]
shutil.copyfile(pathlib.Path(os.environ['FIXTURE_DOWNLOADS']) / url.rsplit('/', 1)[1], destination)
''')
        self.archive = self.downloads / "herdr-tokens-0.2.0-x86_64-unknown-linux-musl.tar.gz"

    @staticmethod
    def executable(path: Path, content: str) -> None:
        path.write_text(content)
        path.chmod(0o755)

    def package(self, *, binary: bool = True) -> None:
        data = b"#!/bin/sh\necho replacement\n"
        with tarfile.open(self.archive, "w:gz") as archive:
            member = tarfile.TarInfo("herdr-tokens" if binary else "unrelated")
            member.size = len(data)
            member.mode = 0o755
            archive.addfile(member, io.BytesIO(data))
        self.checksum()

    def checksum(self) -> None:
        digest = hashlib.sha256(self.archive.read_bytes()).hexdigest()
        (self.downloads / "SHA256SUMS").write_text(f"{digest}  {self.archive.name}\n")

    def run_script(self, name: str) -> subprocess.CompletedProcess[str]:
        return subprocess.run(["sh", str(ROOT / "scripts" / name)], cwd=self.root, env=self.env, capture_output=True, text=True, timeout=10)

    def test_install_failures_preserve_the_previous_executable(self) -> None:
        for failure in ["platform", "missing checksum", "duplicate checksum", "mismatch", "extraction", "missing binary", "staging"]:
            with self.subTest(failure=failure):
                self.env["FIXTURE_PLATFORM"] = "Linux-x86_64"
                (self.bin / "install").unlink(missing_ok=True)
                self.package(binary=failure != "missing binary")
                sums = self.downloads / "SHA256SUMS"
                if failure == "platform":
                    self.env["FIXTURE_PLATFORM"] = "Plan9-mips"
                elif failure == "missing checksum":
                    sums.write_text("a" * 64 + "  another.tar.gz\n")
                elif failure == "duplicate checksum":
                    sums.write_text(sums.read_text() * 2)
                elif failure == "mismatch":
                    self.archive.write_bytes(b"tampered")
                elif failure == "extraction":
                    self.archive.write_bytes(b"not a tar archive")
                    self.checksum()
                elif failure == "staging":
                    self.executable(self.bin / "install", "#!/bin/sh\nexit 1\n")
                result = self.run_script("install-release.sh")
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(subprocess.check_output([self.installed], text=True).strip(), "previous")
                self.assertEqual(list(self.installed.parent.glob(".herdr-tokens.*")), [])

    def test_verified_install_replaces_executable(self) -> None:
        self.package()
        result = self.run_script("install-release.sh")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(subprocess.check_output([self.installed], text=True).strip(), "replacement")

    def setup_start(self) -> Path:
        config = self.root / "config"
        config.mkdir()
        examples = self.root / "examples"
        examples.mkdir()
        (examples / "tokens.toml").write_text("schema_version=1\n# complete default\n")
        self.env.update(HERDR_PLUGIN_ROOT=str(self.root), HERDR_PLUGIN_CONFIG_DIR=str(config))
        self.executable(self.installed, '#!/bin/sh\ncat "$HERDR_PLUGIN_CONFIG_DIR/tokens.toml"\n')
        return config / "tokens.toml"

    def test_concurrent_starts_only_expose_complete_configuration(self) -> None:
        config = self.setup_start()
        processes = [subprocess.Popen(["sh", str(ROOT / "scripts/start.sh")], cwd=self.root, env=self.env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True) for _ in range(12)]
        try:
            for process in processes:
                stdout, stderr = process.communicate(timeout=10)
                self.assertEqual(process.returncode, 0, stderr)
                self.assertEqual(stdout, "schema_version=1\n# complete default\n")
        finally:
            for process in processes:
                if process.poll() is None:
                    process.kill()
                process.wait()
        config.write_text("schema_version=1\n# user config\n")
        self.assertEqual(self.run_script("start.sh").stdout, config.read_text())
        self.assertEqual(list(config.parent.glob(".tokens.toml.*")), [])

    def test_concurrently_created_user_config_wins(self) -> None:
        config = self.setup_start()
        marker = self.root / "copying"
        gate = self.root / "continue"
        self.executable(self.bin / "cp", f'#!/bin/sh\ntouch "{marker}"\nwhile [ ! -f "{gate}" ]; do sleep 0.01; done\nexec /bin/cp "$@"\n')
        process = subprocess.Popen(["sh", str(ROOT / "scripts/start.sh")], cwd=self.root, env=self.env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        try:
            deadline = time.monotonic() + 5
            while not marker.exists():
                self.assertLess(time.monotonic(), deadline)
                time.sleep(.01)
            config.write_text("schema_version=1\n# concurrent user\n")
            gate.touch()
            stdout, stderr = process.communicate(timeout=10)
            self.assertEqual(process.returncode, 0, stderr)
            self.assertEqual(stdout, config.read_text())
        finally:
            if process.poll() is None:
                process.kill()
            process.wait()

    def test_dangling_config_symlink_is_never_followed(self) -> None:
        config = self.setup_start()
        target = self.root / "missing"
        config.symlink_to(target)
        self.assertNotEqual(self.run_script("start.sh").returncode, 0)
        self.assertFalse(target.exists())
        self.assertTrue(config.is_symlink())

    def test_published_assets_are_immutable(self) -> None:
        self.package()
        shutil.copyfile(self.archive, self.root / self.archive.name)
        shutil.copyfile(self.downloads / "SHA256SUMS", self.root / "SHA256SUMS")
        self.env["RELEASE_TAG"] = "v0.2.0"
        self.executable(self.bin / "gh", f'''#!{sys.executable}
import os, pathlib, shutil, sys
if sys.argv[1:3] == ['release', 'view']:
    raise SystemExit(0)
if sys.argv[1:3] == ['release', 'download']:
    target = pathlib.Path(sys.argv[sys.argv.index('--dir')+1])
    for source in pathlib.Path(os.environ['FIXTURE_DOWNLOADS']).iterdir():
        shutil.copyfile(source, target / source.name)
else:
    raise SystemExit('unexpected mutation of existing release')
''')
        result = self.run_script("publish-release.sh")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.archive.write_bytes(b"different published binary")
        self.assertNotEqual(self.run_script("publish-release.sh").returncode, 0)


if __name__ == "__main__":
    unittest.main()
