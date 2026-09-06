#!/usr/bin/env python3
"""Render the binary-only Homebrew formula and checksums from all six release archives."""

import argparse
import hashlib
from pathlib import Path
import re
import tarfile
import zipfile


TARGETS = {
    "macos": ("aarch64-apple-darwin", "x86_64-apple-darwin"),
    "linux": ("aarch64-unknown-linux-musl", "x86_64-unknown-linux-musl"),
}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("version", help="Release version, without the v prefix")
    parser.add_argument("archives", type=Path)
    parser.add_argument("output", type=Path, help="Destination codex-swap.rb")
    args = parser.parse_args()
    if not re.fullmatch(r"\d+\.\d+\.\d+", args.version):
        parser.error("version must be MAJOR.MINOR.PATCH")

    lines = [
        "class CodexSwap < Formula",
        '  desc "Run Codex under different accounts with shared conversation history"',
        '  homepage "https://github.com/maddada/codex-swap"',
        f'  version "{args.version}"',
        '  license "MIT"',
        "",
        "  # CDXC:Release 2026-09-06 DECISION:",
        "  # Users install prebuilt binaries on macOS and Linux without Rust or Cargo.",
    ]
    checksums = []
    for system, targets in TARGETS.items():
        lines.append(f"  on_{system} do")
        if system == "macos":
            lines.extend(["    depends_on macos: :big_sur", ""])
        for arch, target in zip(("arm", "intel"), targets):
            name = f"codex-swap-{args.version}-{target}.tar.gz"
            archive = args.archives / name
            with tarfile.open(archive) as contents:
                members = contents.getmembers()
                if {m.name for m in members} != {
                    "xswap", "LICENSE", "README.md", "THIRD_PARTY_NOTICES.md"
                } or len(members) != 4:
                    raise ValueError(f"Unexpected archive contents: {archive}")
                if not all(m.isfile() for m in members):
                    raise ValueError(f"Archive must contain regular files: {archive}")
                binary = contents.getmember("xswap")
                if not binary.mode & 0o111 or binary.size == 0:
                    raise ValueError(f"Missing executable xswap: {archive}")
            digest = hashlib.sha256(archive.read_bytes()).hexdigest()
            checksums.append(f"{digest}  {name}\n")
            lines.extend([
                f"    on_{arch} do",
                f'      url "https://github.com/maddada/codex-swap/releases/download/v#{{version}}/codex-swap-#{{version}}-{target}.tar.gz"',
                f'      sha256 "{digest}"',
                "    end",
                "",
            ])
        lines.pop()
        lines.extend(["  end", ""])
    for target in ("x86_64-pc-windows-msvc", "aarch64-pc-windows-msvc"):
        name = f"codex-swap-{args.version}-{target}.zip"
        archive = args.archives / name
        with zipfile.ZipFile(archive) as contents:
            members = contents.infolist()
            if {m.filename for m in members} != {
                "xswap.exe", "LICENSE", "README.md", "THIRD_PARTY_NOTICES.md"
            } or len(members) != 4 or any(m.is_dir() for m in members):
                raise ValueError(f"Unexpected Windows archive contents: {archive}")
            if contents.getinfo("xswap.exe").file_size == 0:
                raise ValueError(f"Missing executable xswap.exe: {archive}")
        checksums.append(f"{hashlib.sha256(archive.read_bytes()).hexdigest()}  {name}\n")
    installer = args.archives / "install.ps1"
    if not installer.is_file() or installer.stat().st_size == 0:
        raise ValueError("Missing Windows installer: install.ps1")
    checksums.append(f"{hashlib.sha256(installer.read_bytes()).hexdigest()}  install.ps1\n")
    lines.extend([
        "  def install",
        '    bin.install "xswap"',
        '    doc.install "THIRD_PARTY_NOTICES.md"',
        "  end",
        "",
        "  def caveats",
        "    <<~EOS",
        "      Install the official Codex CLI separately and make sure codex is on PATH.",
        "      Get started: codex login, then xswap add --alias personal",
        "    EOS",
        "  end",
        "end",
        "",
    ])
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text("\n".join(lines))
    (args.archives / "SHA256SUMS").write_text("".join(sorted(checksums)))


if __name__ == "__main__":
    main()
