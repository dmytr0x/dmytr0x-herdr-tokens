#!/usr/bin/env python3
"""Check local Markdown links and the structure of shipped service examples."""
import configparser
from pathlib import Path
import plistlib
import re
import shlex
import subprocess
import tomllib
from urllib.parse import unquote, urlsplit

ROOT = Path(__file__).resolve().parents[1]


def main() -> None:
    documents = sorted(ROOT.glob("*.md")) + sorted((ROOT / "docs").rglob("*.md"))
    for document in documents:
        for target in re.findall(r"\[[^\]]*\]\(([^)]+)\)", document.read_text(encoding="utf-8")):
            link = urlsplit(target.strip("<>"))
            if link.scheme or link.netloc or not link.path:
                continue
            path = document.parent / unquote(link.path)
            assert path.exists(), f"broken local link in {document.name}: {target}"
    with (ROOT / "examples/launchd.plist").open("rb") as stream:
        launchd = plistlib.load(stream)
    assert launchd["ProgramArguments"][1] == "run"
    assert launchd["ExitTimeOut"] >= 7
    systemd = configparser.ConfigParser(interpolation=None)
    systemd.read(ROOT / "examples/systemd.service")
    assert shlex.split(systemd["Service"]["ExecStart"])[1] == "run"
    assert int(systemd["Service"]["TimeoutStopSec"]) >= 7
    for example in (ROOT / "examples").glob("*.toml"):
        with example.open("rb") as stream:
            tomllib.load(stream)
    assert (ROOT / "examples/ci-status").is_file()
    subprocess.run(["sh", "-n", str(ROOT / "examples/ci-status")], check=True)
    print(f"PASS: {len(documents)} documents and shipped example structure")


if __name__ == "__main__":
    main()
