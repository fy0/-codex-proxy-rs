#!/usr/bin/env python3
"""将 Image Bridge 回归用例注入固定版本的官方 CPR Provider 与 Runtime 测试目标。"""

from __future__ import annotations

import argparse
from pathlib import Path
import subprocess
import sys

HOST_REVISION = "96e8443ede051abdd8f0bac69462a7cfe2945633"
PROVIDER_DIRECTORY = Path("backend/crates/providers/openai/tests/provider/contract")
RUNTIME_DIRECTORY = Path("backend/crates/gateway-plugin/runtime/src/callback")


def prepare_module(
    host_root: Path,
    module_path: Path,
    test_path: Path,
    name: str,
    anchor: bytes,
    *,
    private_test_module: bool = False,
) -> tuple[Path, bytes, Path, bytes]:
    module_path = host_root / module_path
    test_path = host_root / test_path
    source = module_path.read_bytes()
    asset = Path(__file__).resolve().with_name(f"{name}.rs").read_bytes()
    declaration = f"mod {name};".encode()
    source_lines = source.splitlines()
    if source_lines.count(anchor) != 1:
        raise RuntimeError(f"official module anchor changed: {module_path}")
    if source_lines.count(declaration) > 1:
        raise RuntimeError(f"duplicate injected module declaration: {name}")
    if test_path.exists() and test_path.read_bytes() != asset:
        raise RuntimeError(f"refusing to overwrite a different contract test: {test_path}")

    newline = b"\r\n" if b"\r\n" in source else b"\n"
    block = (
        b"#[cfg(test)]" + newline + declaration
        if private_test_module
        else declaration
    )
    patched = source
    if declaration in source_lines:
        if block not in source:
            raise RuntimeError(f"injected module declaration does not match: {name}")
    else:
        replacement = (
            block + newline + newline + anchor + newline
            if private_test_module
            else anchor + newline + block + newline
        )
        patched = source.replace(anchor + newline, replacement, 1)
        if patched == source:
            raise RuntimeError(f"official module anchor has no supported line ending: {name}")
    return module_path, patched, test_path, asset


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

    # 所有宿主文件先完成预检；--check 不创建目录，也不写入宿主源码。
    plans = [
        prepare_module(
            host_root,
            PROVIDER_DIRECTORY / "mod.rs",
            PROVIDER_DIRECTORY / "image_bridge_callback_contract.rs",
            "image_bridge_callback_contract",
            b"mod image_model_access;",
        ),
        prepare_module(
            host_root,
            RUNTIME_DIRECTORY / "model.rs",
            RUNTIME_DIRECTORY / "model/image_bridge_capacity_contract.rs",
            "image_bridge_capacity_contract",
            b"fn encode_events(events: Vec<ProviderEvent>) -> Result<(Vec<u8>, u32), PluginFault> {",
            private_test_module=True,
        ),
    ]
    if check:
        print(f"provider and runtime contract preflight passed for {HOST_REVISION}")
        return
    for module_path, patched, test_path, asset in plans:
        if not test_path.exists():
            test_path.parent.mkdir(parents=True, exist_ok=True)
            test_path.write_bytes(asset)
        if module_path.read_bytes() != patched:
            module_path.write_bytes(patched)
    print(f"installed provider and runtime contract tests for {HOST_REVISION}")


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
