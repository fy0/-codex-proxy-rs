#!/usr/bin/env python3
"""Adapt cpr-jzjc's pinned Linux x86_64 offline runtime to Linux aarch64.

Run after `crane pull --platform linux/arm64 <pinned-image> <base-tar>`.
This edits only the ephemeral Action checkout, never upstream sources.
"""
from __future__ import annotations

import hashlib
import json
import os
from pathlib import Path
import re
import tarfile

root = Path(os.environ.get('JZJC_DIR', 'jzjc')).resolve()
base = root / '.cache/runtime/python-base.tar'
assert base.is_file(), f'Missing ARM64 python image at {base}'
with tarfile.open(base) as tar:
    docker_manifest = json.load(tar.extractfile('manifest.json'))
    assert len(docker_manifest) == 1, 'Unexpected docker manifest layout'
    config_bytes = tar.extractfile(docker_manifest[0]['Config']).read()
    config = json.loads(config_bytes)
    assert config['architecture'] == 'arm64' and config['os'] == 'linux', config
    config_hash = 'sha256:' + hashlib.sha256(config_bytes).hexdigest()

runtime_py = root / 'scripts/build_runtime.py'
src = runtime_py.read_text()
original = src
src, n = re.subn(r'^CONFIG = "sha256:[0-9a-f]{64}"$',
                 f'CONFIG = "{config_hash}"', src, count=1, flags=re.MULTILINE)
assert n == 1, 'Pinned runtime config has changed upstream'
assert '"linux/amd64"' in src and '"amd64"' in src
assert 'x86_64-linux-gnu' in src and 'ld-linux-x86-64.so.2' in src
assert 'manylinux_2_27_x86_64' in src and 'manylinux_2_28_x86_64' in src
src = (src.replace('"linux/amd64"', '"linux/arm64"')
          .replace('"amd64"', '"arm64"')
          .replace('x86_64-linux-gnu', 'aarch64-linux-gnu')
          .replace('ld-linux-x86-64.so.2', 'ld-linux-aarch64.so.1')
          .replace('manylinux_2_27_x86_64', 'manylinux_2_27_aarch64')
          .replace('manylinux_2_28_x86_64', 'manylinux_2_28_aarch64'))
assert src != original
runtime_py.write_text(src)

for path in (root / 'backend/src/engine.rs', root / 'scripts/verify_artifact.py'):
    src = path.read_text()
    assert 'x86_64-linux-gnu' in src and 'ld-linux-x86-64.so.2' in src, path
    new = src.replace('x86_64-linux-gnu', 'aarch64-linux-gnu').replace(
        'ld-linux-x86-64.so.2', 'ld-linux-aarch64.so.1')
    path.write_text(new)

# SHA256 from the PyPI JSON metadata for numpy==2.4.3 cp311 aarch64
# numpy-2.4.3-cp311-cp311-manylinux_2_27_aarch64.manylinux_2_28_aarch64.whl
pin = 'a016db5c5dba78fa8fe9f5d80d6708f9c42ab087a739803c0ac83a43d686a470'
(root / 'scripts/numpy-wheel.sha256').write_text(pin + '\n')
print(json.dumps({'runtimeImageConfigSha256': config_hash,
                  'arch': 'aarch64', 'numpyWheelSha256': pin}, indent=2))
