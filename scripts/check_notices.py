#!/usr/bin/env python3
"""Check shipped notices against checksum-verified crates in an active release graph."""

import argparse
import hashlib
import os
from pathlib import Path
import re
import subprocess
import tarfile
import tomllib


ROOT = Path(__file__).resolve().parents[1]


def required_texts(crate, package):
    """Select MIT where offered; retain additional and copied-code notices verbatim."""
    files = {m.name.split("/", 1)[1]: m for m in crate.getmembers() if m.isfile()}
    paths = {p.upper(): p for p in files}
    manifest = tomllib.loads(crate.extractfile(files["Cargo.toml"]).read().decode())
    license_expression = manifest["package"].get("license", "")
    if package == "ring":
        selected = {p for p in files if Path(p).name.upper().startswith("LICENSE")}
    else:
        mit_offered = "MIT" in license_expression and (
            "AND" not in license_expression
            or license_expression == "(MIT OR Apache-2.0) AND Unicode-3.0"
        )
        if mit_offered and "LICENSE-MIT" in paths:
            primary = paths["LICENSE-MIT"]
        elif license_expression == "Apache-2.0 OR BSL-1.0":
            primary = "LICENSE-BOOST"
        elif license_expression in {
            "MIT", "ISC", "BSD-3-Clause", "Unicode-3.0", "CDLA-Permissive-2.0",
            "Apache-2.0", "MIT OR Apache-2.0",
        }:
            primary = next((paths[p] for p in ("LICENSE", "LICENSE.TXT") if p in paths), None)
        else:
            raise ValueError(f"Review the license selection for {package}: {license_expression}")
        if not primary:
            raise ValueError(f"Missing license text for {package}")
        selected = {primary}
        selected.update(p for p in files if (
            ("/" in p and Path(p).name.upper().startswith(("LICENSE", "NOTICE", "COPYING")))
            or Path(p).name.upper().startswith(("NOTICE", "COPYRIGHT", "LICENSE-THIRD", "LICENSE-UNICODE"))
        ))
    for path in sorted(selected):
        yield path, crate.extractfile(files[path]).read().decode().replace("\r\n", "\n")
    if package == "ring":
        # ISC per-file notices and BoringSSL/assembly/copy attributions accompany the full licenses.
        comments = re.compile(r"/\*.*?\*/|(?m:^(?://[^\n]*\n)+)|(?m:^(?:\#[^\n]*\n)+)", re.DOTALL)
        for path, member in sorted(files.items()):
            if not path.endswith((".rs", ".c", ".h", ".S", ".pl", ".asm")):
                continue
            source = crate.extractfile(member).read().decode().replace("\r\n", "\n")
            for comment in comments.findall(source):
                if "copyright" in comment.lower() or "Permission is hereby granted" in comment:
                    yield path, comment


def check(target):
    tree = subprocess.check_output([
        "cargo", "tree", "--locked", "--target", target, "-e", "normal",
        "--prefix", "none", "--format", "{p}",
    ], cwd=ROOT, text=True)
    packages = set(re.findall(r"^(\S+) v(\S+)", tree, re.MULTILINE))
    lock = tomllib.loads((ROOT / "Cargo.lock").read_text(encoding="utf-8"))
    locked = {(p["name"], p["version"]): p for p in lock["package"]}
    project = tomllib.loads((ROOT / "Cargo.toml").read_text(encoding="utf-8"))["package"]
    packages.remove((project["name"], project["version"]))
    bundle = (ROOT / "THIRD_PARTY_NOTICES.md").read_text(encoding="utf-8")
    cache = Path(os.environ.get("CARGO_HOME", Path.home() / ".cargo")) / "registry" / "cache"
    checked = 0
    for name, version in sorted(packages):
        package = locked[name, version]
        if package.get("source") != "registry+https://github.com/rust-lang/crates.io-index":
            raise ValueError(f"Review notice provenance for {name} {version}")
        checksum = package["checksum"]
        archives = list(cache.glob(f"*/{name}-{version}.crate"))
        archive = next((
            p for p in archives if hashlib.sha256(p.read_bytes()).hexdigest() == checksum
        ), None)
        if not archive:
            raise ValueError(f"Missing checksum-verified crate: {name} {version}")
        if f"| {name} | {version} | {checksum} |" not in bundle:
            raise ValueError(f"Missing notice provenance: {name} {version}")
        with tarfile.open(archive) as crate:
            for path, text in required_texts(crate, name):
                if not text or text not in bundle:
                    raise ValueError(f"Missing exact notice text: {name} {version}/{path}")
        checked += 1
    if not checked:
        raise ValueError("No dependencies were checked")
    print(f"Verified notices for {checked} active dependencies on {target}")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("target", help="Release target triple")
    check(parser.parse_args().target)
