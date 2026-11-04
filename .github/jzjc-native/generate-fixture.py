#!/usr/bin/env python3
"""Build an offline Python/NumPy parity fixture on the GitHub runner only."""
from pathlib import Path
import json
import sys

repo = Path(sys.argv[1]).resolve()
out = Path(sys.argv[2]).resolve()
sys.path.insert(0, str(repo / "vendor/modeltrace"))
import fingerprint as fp

bank = json.loads((repo / "vendor/modeltrace/data/unified_bank.json").read_text())
rows = []
for case, length in enumerate((292, 307, 321, 330, 315, 298)):
    seed = (17, 73, 281, 389, 101, 213)[case]
    numbers = [
        ((i * i * (case + 1) + i * 97 + (i % 13) * case * 59 + seed * 29) % 355) + 1
        for i in range(length)
    ]
    result = fp.analyze_global_outputs(
        [{"text": ",".join(str(i) for i in numbers), "expected_count": length}], bank
    )
    rows.append({
        "numbers": numbers,
        "prediction": result["prediction"],
        "results": [{"model": item["model"], "probability": item["probability"]}
                    for item in result["results"]],
    })
out.write_text(json.dumps(rows, ensure_ascii=False), encoding="utf-8")
print(f"Generated {len(rows)} independent native/Python parity samples at {out}")
