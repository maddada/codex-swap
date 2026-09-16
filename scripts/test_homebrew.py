#!/usr/bin/env python3
"""Exercise the four-member release contract using disposable synthetic archives."""

import hashlib
import io
from pathlib import Path
import stat
import subprocess
import sys
import tarfile
import tempfile
import unittest
import zipfile

import homebrew


ROOT = Path(__file__).resolve().parents[1]
VERSION = "0.3.1"
TARGETS = [t for targets in homebrew.TARGETS.values() for t in targets] + [
    "x86_64-pc-windows-msvc", "aarch64-pc-windows-msvc",
]


class ReleaseArchives(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.directory = Path(self.temp.name)
        self.archives = []
        for target in TARGETS:
            windows = target.endswith("msvc")
            name = f"codex-swap-{VERSION}-{target}." + ("zip" if windows else "tar.gz")
            archive = self.directory / name
            self.archives.append(archive)
            self.write_archive(archive, self.entries(windows))
        (self.directory / "install.ps1").write_bytes((ROOT / "scripts/install.ps1").read_bytes())

    def entries(self, windows):
        return [("xswap.exe" if windows else "xswap", b"executable fixture", "file")] + [
            (name, (ROOT / name).read_bytes(), "file")
            for name in ("LICENSE", "README.md", "THIRD_PARTY_NOTICES.md")
        ]

    def write_archive(self, archive, entries):
        if archive.suffix == ".zip":
            with zipfile.ZipFile(archive, "w", zipfile.ZIP_DEFLATED) as output:
                for name, payload, kind in entries:
                    member = zipfile.ZipInfo(name)
                    member.create_system = 3
                    member.external_attr = ((stat.S_IFLNK if kind == "link" else stat.S_IFREG) | 0o755) << 16
                    output.writestr(member, payload)
        else:
            with tarfile.open(archive, "w:gz") as output:
                for name, payload, kind in entries:
                    member = tarfile.TarInfo(name)
                    member.mode = 0o755
                    member.type = tarfile.SYMTYPE if kind == "link" else tarfile.REGTYPE
                    member.linkname = "xswap" if kind == "link" else ""
                    member.size = 0 if kind == "link" else len(payload)
                    output.addfile(member, io.BytesIO(payload) if kind != "link" else None)

    def assemble(self):
        return subprocess.run([
            sys.executable, str(ROOT / "scripts/homebrew.py"), VERSION,
            str(self.directory), str(self.directory / "codex-swap.rb"),
        ], capture_output=True, text=True)

    def test_documents_formula_and_checksums(self):
        # Windows checkouts may use CRLF; the full legal text must still match.
        entries = [(n, p.replace(b"\n", b"\r\n") if n.endswith(".md") else p, k) for n, p, k in self.entries(True)]
        self.write_archive(self.archives[-1], entries)
        result = self.assemble()
        self.assertEqual(result.returncode, 0, result.stderr)
        formula = (self.directory / "codex-swap.rb").read_text()
        self.assertIn('doc.install "LICENSE", "THIRD_PARTY_NOTICES.md"', formula)
        lines = (self.directory / "SHA256SUMS").read_text().splitlines()
        self.assertEqual(len(lines), 7)
        for archive in self.archives + [self.directory / "install.ps1"]:
            digest = hashlib.sha256(archive.read_bytes()).hexdigest()
            self.assertIn(f"{digest}  {archive.name}", lines)
            if archive.suffix != ".zip" and archive.name != "install.ps1":
                self.assertIn(f'sha256 "{digest}"', formula)
        syntax = subprocess.run(["ruby", "-c", str(self.directory / "codex-swap.rb")], capture_output=True, text=True)
        self.assertEqual(syntax.returncode, 0, syntax.stderr)

    def test_rejects_missing_unexpected_escaping_and_link_members(self):
        for archive in (self.archives[0], self.archives[-1]):
            windows = archive.suffix == ".zip"
            entries = self.entries(windows)
            malformed = {
                "missing notices": entries[:-1],
                "extra member": entries + [("unexpected", b"extra", "file")],
                "duplicate": entries + [entries[-1]],
                "escaping path": entries[:-1] + [("../THIRD_PARTY_NOTICES.md", entries[-1][1], "file")],
                "link": entries[:-1] + [(entries[-1][0], b"xswap", "link")],
                "empty binary": [(entries[0][0], b"", "file")] + entries[1:],
                "empty notices": entries[:-1] + [(entries[-1][0], b"", "file")],
                "incomplete notices": entries[:-1] + [(entries[-1][0], b"reference-only notices", "file")],
                "wrong license": [entries[0], ("LICENSE", b"different license", "file")] + entries[2:],
            }
            for reason, members in malformed.items():
                with self.subTest(archive=archive.name, reason=reason):
                    self.write_archive(archive, members)
                    self.assertNotEqual(self.assemble().returncode, 0)
            self.write_archive(archive, entries)


if __name__ == "__main__":
    unittest.main()
