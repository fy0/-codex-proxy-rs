#!/usr/bin/env python3
"""Verify package layout, hashes, ELF architecture and unbounded host version.

No compilation or real API calls. Run on the Linux GitHub Actions build runner.
"""
import hashlib
import json
import struct
from pathlib import Path
import sys
import tarfile
import tempfile

package=Path(sys.argv[1]).resolve()
target=sys.argv[2]
expected_machines={"aarch64-unknown-linux-gnu":183,"x86_64-unknown-linux-gnu":62}
assert target in expected_machines
assert package.name==f"mr-xiongyq.quota-guard-0.2.4-{target}.tar.gz", package.name
assert package.stat().st_size < 8*1024*1024, "Quota guard unexpectedly large"
digest=hashlib.sha256(package.read_bytes()).hexdigest()
assert package.with_name(package.name+".sha256").read_text().split()[0]==digest
with tempfile.TemporaryDirectory(prefix="verify-cpr-quota-") as folder:
    root=Path(folder)
    with tarfile.open(package,"r:gz") as archive:
        for member in archive:
            assert not Path(member.name).is_absolute(), member.name
            assert ".." not in Path(member.name).parts, member.name
            assert member.isfile() or member.isdir(), member.name
        archive.extractall(root,filter="data")
    manifest=json.loads((root/"plugin.json").read_text(encoding="utf-8"))
    assert manifest["publisher"]=="mr-xiongyq"
    assert manifest["name"]=="quota-guard"
    assert manifest["version"]=="0.2.4"
    assert manifest["manifestVersion"]==2
    assert manifest["engines"]["codex-proxy-rs"]==">=0.0.0"
    assert manifest["package"]["protocolVersion"]==7
    assert manifest["package"]["target"]["os"]=="linux"
    assert manifest["package"]["target"]["architecture"]==("aarch64" if target.startswith("aarch64") else "x86_64")
    assert set(manifest["package"]["files"])=={manifest["main"],*manifest["resources"]}
    for name,sha in manifest["package"]["files"].items():
        assert hashlib.sha256((root/name).read_bytes()).hexdigest()==sha, name
    binary=(root/manifest["main"]).read_bytes()
    assert binary[:4]==b"\x7fELF" and binary[4]==2 and binary[5]==1
    assert struct.unpack_from("<H",binary,18)[0]==expected_machines[target]
    assert len(binary)>100_000
print(json.dumps({"plugin":"quota-guard","target":target,"size":package.stat().st_size,
                  "checksum":digest,"manifestVersion":2,"protocolVersion":7,
                  "engineRange":">=0.0.0","verified":True},indent=2))
