#!/usr/bin/env python3
"""Validate native Linux x86_64/AArch64 ModelTrace installation and exercise a mock-host probe."""
import hashlib
import json
from pathlib import Path
import struct
import sys
import tarfile
import tempfile

target = sys.argv[3]
machines = {"aarch64-unknown-linux-gnu": 183, "x86_64-unknown-linux-gnu": 62}
assert target in machines, f"Unsupported target {target}"
package = Path(sys.argv[1]).resolve()
assert package.is_file(), "No plugin installation archive produced"
digest_file = package.with_name(package.name + ".sha256")
digest = hashlib.sha256(package.read_bytes()).hexdigest()
assert digest_file.read_text().split()[0] == digest
assert package.stat().st_size < 12 * 1024 * 1024, "Native plugin archive unexpectedly large"
with tempfile.TemporaryDirectory(prefix="cpr-native-340-") as scratch:
    root = Path(scratch)
    with tarfile.open(package, "r:gz") as archive:
        for member in archive:
            assert not Path(member.name).is_absolute()
            assert ".." not in Path(member.name).parts
            assert member.isfile() or member.isdir()
        archive.extractall(root, filter="data")
    manifest = json.loads((root / "plugin.json").read_text())
    assert manifest["publisher"] == "personal"
    assert manifest["name"] == "modeltrace-health"
    assert manifest["version"] == "1.15.1"
    assert manifest["manifestVersion"] == 2
    assert manifest["package"]["protocolVersion"] == 7
    assert manifest["engines"]["codex-proxy-rs"] == ">=0.0.0"
    assert not any(name.startswith("runtime/") for name in manifest["resources"])
    assert set(manifest["package"]["files"]) == {manifest["main"], *manifest["resources"]}
    for path, sha in manifest["package"]["files"].items():
        assert hashlib.sha256((root / path).read_bytes()).hexdigest() == sha, path
    elf = (root / manifest["main"]).read_bytes()
    assert elf[:4] == b"\x7fELF" and elf[4] == 2 and elf[5] == 1
    assert struct.unpack_from("<H", elf, 18)[0] == machines[target], f"wrong ELF machine for {target}"
    assert b"libpython3.11" not in elf and b"modeltrace-runtime-" not in elf
    # End-to-end test against the real protocol mock (without real model/API credentials).
    sys.path.insert(0, str(Path(sys.argv[2]).resolve() / "scripts"))
    from protocol_smoke import Host
    host = Host(binary=root / manifest["main"])
    try:
        bootstrap = host.request("api/bootstrap")
        config = bootstrap["config"]
        config.update(
            keyId="key-test", requestModel="gpt-6-sol",
            reasoningEffort="none", confirmationRounds=1,
            lowConfidencePercent=0, degradedPercent=100, retryDelaySeconds=0,
        )
        host.request("api/config", {"config": config, "revision": bootstrap["configRevision"]})
        job = host.request(
            "api/submit", {"accountIds": ["acct-test"], "requestId": "native-340-integration"}
        )["submitted"][0]
        host.tick(2)
        report = host.request("api/report?id=" + job)
        assert report["status"] == "completed", report.get("message")
        assert len(report["rounds"]) == 1 and len(host.executions) == 1, report
    finally:
        host.close()
print(json.dumps({
    "artifact": package.name,
    "bytes": package.stat().st_size,
    "sha256": digest,
    "target": target,
    "nativeRuntime": True,
    "protocolVersion": 7,
    "mockIntegration": "passed",
}, indent=2))
