#!/usr/bin/env python3
"""Compact an ARM64 JZJC private Python/NumPy runtime without changing its API.

Runs only in a GitHub Actions ARM64 runner after build_runtime.py. Removes
runtime-irrelevant headers, compiler support, type stubs and development-only
modules, and preserves all native ELF libraries byte-for-byte. Leaves the rest intact.
"""
from __future__ import annotations

import argparse
import gzip
import io
import os
from pathlib import Path, PurePosixPath
import tarfile
import tempfile


def remove_development_only(name: str) -> bool:
    path = PurePosixPath(name)
    parts = path.parts
    if not name.startswith(("usr/local/lib/python3.11/", "usr/local/lib/python3.11/site-packages/")):
        return False
    if name.startswith("usr/local/lib/python3.11/site-packages/numpy/"):
        if any(part in {"f2py", "include", "typing", "doc", "tests"} for part in parts):
            return True
        if path.suffix in {".pyi", ".pxi", ".pxd", ".pyx", ".h", ".hpp", ".c", ".cc", ".cpp", ".a", ".lib"}:
            return True
    if name.startswith("usr/local/lib/python3.11/") and any(part in {"lib2to3", "pydoc_data", "unittest"} for part in parts):
        return True
    return False


def compact(path: Path) -> None:
    before = path.stat().st_size
    original = {}
    with tarfile.open(path, "r:gz") as archive:
        for member in archive:
            if not member.isfile() or member.name.startswith("/") or ".." in PurePosixPath(member.name).parts:
                raise ValueError(f"Unsafe unexpected member {member.name}")
            original[member.name] = (archive.extractfile(member).read(), member.mode)
    kept = {}
    omitted = []
    # Never alter ELF binaries: GNU strip can invalidate PT_LOAD alignment on AArch64.
    for name, (data, mode) in original.items():
        if remove_development_only(name):
            omitted.append((name, len(data)))
            continue
        kept[name] = data, mode
    with tempfile.NamedTemporaryFile(prefix="jzjc-runtime-", suffix=".tar.gz", dir=path.parent, delete=False) as tmp:
        destination = Path(tmp.name)
    try:
        with destination.open("wb") as raw:
            with gzip.GzipFile(fileobj=raw, mode="wb", mtime=0, compresslevel=9) as gz:
                with tarfile.open(fileobj=gz, mode="w|") as out:
                    for name, (data, mode) in sorted(kept.items()):
                        info = tarfile.TarInfo(name)
                        info.mode = mode
                        info.size = len(data)
                        info.mtime = 0
                        out.addfile(info, io.BytesIO(data))
        # Sanity: the archive remains a valid GZIP tar; no surviving member changed names.
        with tarfile.open(destination, "r:gz") as new_tar:
            names = [e.name for e in new_tar]
        if names != sorted(kept):
            raise AssertionError("Archive round-trip failed")
        os.replace(destination, path)
    finally:
        destination.unlink(missing_ok=True)
    after = path.stat().st_size
    print(f"Runtime compression: {before:,} -> {after:,} bytes (saved {before-after:,}); {len(omitted)} development-only files removed ({sum(x[1] for x in omitted):,} raw bytes); native ELF binaries preserved unchanged")
    # Extra detail for follow-up size budgets. Never print the contents of files.
    for name, size in sorted(omitted, key=lambda t: t[1], reverse=True)[:8]:
        print(f"  omitted {size:>10} {name}")
    if after >= before:
        raise AssertionError("Compaction did not reduce the archive size")


if __name__ == "__main__":
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("archive", type=Path)
    compact(ap.parse_args().archive)
