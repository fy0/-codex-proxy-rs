#!/usr/bin/env python3
"""Apply the native Images management bridge to its exact reviewed Core base."""
from pathlib import Path
import argparse
import hashlib
import json
import subprocess
import sys

def normalized_hash(path):
    return hashlib.sha256(path.read_bytes().replace(b"\r\n", b"\n")).hexdigest()

def git(root, *args):
    return subprocess.run(
        ["git", "-C", str(root), *args],
        check=True, capture_output=True, text=True, encoding="utf-8",
    ).stdout.strip()

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("core", type=Path, help="separate checkout of the exact official Core base")
    parser.add_argument("--check", action="store_true", help="validate compatibility without writing")
    args = parser.parse_args()
    root = args.core.resolve()
    bundle = Path(__file__).resolve().parent
    manifest = json.loads((bundle / "manifest.json").read_text(encoding="utf-8"))
    actual_base = git(root, "rev-parse", "HEAD")
    if actual_base != manifest["baseCommit"]:
        raise RuntimeError(f"Unsupported Core base: {actual_base}; required {manifest['baseCommit']}. No files changed.")
    states = {}
    for relative, hashes in manifest["files"].items():
        path = root / relative
        if not path.is_file() or path.is_symlink():
            raise RuntimeError(f"Required regular source file missing: {relative}. No files changed.")
        digest = normalized_hash(path)
        if digest == hashes["before"]:
            states[relative] = "before"
        elif digest == hashes["after"]:
            states[relative] = "after"
        else:
            raise RuntimeError(f"Source hash mismatch: {relative}. No files changed.")
    if set(states.values()) == {"after"}:
        print(json.dumps({"status": "already_applied", "baseCommit": actual_base, "files": len(states)}))
        return
    if set(states.values()) != {"before"}:
        raise RuntimeError("Partial application detected. Use a clean separate Core checkout. No files changed.")
    if git(root, "status", "--porcelain", "--untracked-files=no"):
        raise RuntimeError("Tracked Core source has local changes. Use a clean separate checkout. No files changed.")
    patch = bundle / "native-images.patch"
    git(root, "apply", "--check", str(patch))
    if not args.check:
        git(root, "apply", str(patch))
        for relative, hashes in manifest["files"].items():
            if normalized_hash(root / relative) != hashes["after"]:
                raise RuntimeError(f"Post-apply verification failed: {relative}; do not build this checkout.")
    print(json.dumps({
        "status": "compatible" if args.check else "applied",
        "baseCommit": actual_base,
        "files": len(states),
        "bridgeMethod": "models.images.generate",
        "bridgeVersion": 2,
        "route": "/api/admin/plugins/extensions/{instance_id}/{artifact_sha256}/{revision}/models/images/generations",
    }))

if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError, RuntimeError, subprocess.CalledProcessError) as error:
        print(f"native-images patch: {error}", file=sys.stderr)
        sys.exit(1)
