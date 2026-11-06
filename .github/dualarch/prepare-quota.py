#!/usr/bin/env python3
"""Apply version-unbounded manifest changes to pinned quota-guard v0.2.3.

The original plugin logic and its SDK remain unchanged; the package is versioned
as 0.2.4 to leave the old, already released archive immutable.
"""
import json
from pathlib import Path
import sys

repo=Path(sys.argv[1]).resolve()
p=repo/"plugin.json"
data=json.loads(p.read_text(encoding="utf-8"))
assert data["publisher"]=="mr-xiongyq" and data["name"]=="quota-guard"
assert data["version"]=="0.2.3"
assert data["engines"]["codex-proxy-rs"]==">=3.35.1, <4.0.0"
data["version"]="0.2.4"
# The SDK manifest parser requires an explicit comparator rather than '*'.
# >=0.0.0 is semver's effectively unlimited stable-version range.
data["engines"]["codex-proxy-rs"]=">=0.0.0"
p.write_text(json.dumps(data,ensure_ascii=False,indent=2)+"\n",encoding="utf-8")
p=repo/"Cargo.toml"
s=p.read_text(encoding="utf-8")
old='name = "cpr-quota-guard"\nversion = "0.2.3"'
assert s.count(old)==1
p.write_text(s.replace(old,'name = "cpr-quota-guard"\nversion = "0.2.4"'),encoding="utf-8")
p=repo/"Cargo.lock"
s=p.read_text(encoding="utf-8")
assert s.count(old)==1
p.write_text(s.replace(old,'name = "cpr-quota-guard"\nversion = "0.2.4"'),encoding="utf-8")
print("Quota guard version 0.2.4, engine >=0.0.0, pinned SDK unchanged")
