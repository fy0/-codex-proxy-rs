#!/usr/bin/env python3
"""将 Image Bridge 回归用例注入固定版本的官方 CPR Provider 测试目标。"""

from __future__ import annotations

import argparse
from pathlib import Path
import subprocess
import sys

HOST_REVISION = "96e8443ede051abdd8f0bac69462a7cfe2945633"
CONTRACT_DIRECTORY = Path("backend/crates/providers/openai/tests/provider/contract")
MODULE_NAME = "image_bridge_callback_contract"
ANCHOR = b"mod image_model_access;"


def install(host_root: Path, check: bool) -> None:
    host_root = host_root.resolve(strict=True)
    revision = subprocess.check_output(
        ["git", "-C", str(host_root), "rev-parse", "HEAD"],
        text=True,
    ).strip()
    if revision != HOST_REVISION:
        raise RuntimeError(
            f"official host revision must be {HOST_REVISION}, found {revision}"
        )

    directory = host_root / CONTRACT_DIRECTORY
    module_path = directory / "mod.rs"
    test_path = directory / f"{MODULE_NAME}.rs"
    asset_path = Path(__file__).resolve().with_name(f"{MODULE_NAME}.rs")
    source = module_path.read_bytes()
    asset = asset_path.read_bytes()
    declaration = f"mod {MODULE_NAME};".encode()
    source_lines = source.splitlines()
    if source_lines.count(ANCHOR) != 1:
        raise RuntimeError("official image_model_access module anchor changed")
    if source_lines.count(declaration) > 1:
        raise RuntimeError("duplicate injected module declaration")
    if test_path.exists() and test_path.read_bytes() != asset:
        raise RuntimeError("refusing to overwrite a different contract test module")

    newline = b"\r\n" if b"\r\n" in source else b"\n"
    patched = source
    if declaration not in source_lines:
        patched = source.replace(
            ANCHOR + newline,
            ANCHOR + newline + declaration + newline,
            1,
        )
        if patched == source:
            raise RuntimeError("official module anchor has no supported line ending")

    # 先完成两个目标文件的预检；--check 只检查，不写宿主源码。
    if check:
        print(f"contract test preflight passed for {HOST_REVISION}")
        return
    if not test_path.exists():
        test_path.write_bytes(asset)
    if patched != source:
        module_path.write_bytes(patched)
    print(f"installed {MODULE_NAME} for {HOST_REVISION}")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--host-root", required=True, type=Path)
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    try:
        install(args.host_root, args.check)
    except (OSError, RuntimeError, subprocess.CalledProcessError) as error:
        print(f"contract test installation failed: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
