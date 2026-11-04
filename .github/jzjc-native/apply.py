#!/usr/bin/env python3
"""Apply 1.15.0 native-scoring overlays to a clean Victor-Tau/cpr-jzjc v1.14.0 checkout.

Edits happen exclusively on the GitHub Actions runner. This script is deliberately
strict: changed upstream source must fail rather than silently ship an invalid package.
"""
from pathlib import Path
import json
import re
import shutil
import sys

repo = Path(sys.argv[1]).resolve()
overlays = Path(__file__).resolve().parent
assert (repo / "plugin.json").is_file()
assert (repo / "backend/src/engine.rs").is_file()

def edit(relative: str, fn):
    p = repo / relative
    before = p.read_text(encoding="utf-8")
    after = fn(before)
    assert after != before, f"Expected modification absent: {relative}"
    p.write_text(after, encoding="utf-8", newline="\n")

def replace_once(s: str, before: str, after: str):
    assert s.count(before) == 1, f"Expected exactly one anchor: {before[:80]!r}, got {s.count(before)}"
    return s.replace(before, after)

for file in ("engine.rs", "native_score.rs"):
    shutil.copyfile(overlays / file, repo / "backend/src" / file)

edit("backend/src/main.rs", lambda s: replace_once(s, "mod engine;\n", "mod engine;\nmod native_score;\n"))

def cargo(s):
    s = replace_once(s, 'version = "1.14.0"', 'version = "1.15.0"')
    s = replace_once(s, 'rev = "e0b7a841ca074f17e11dad73f39a159c75cb2fb1"',
                     'rev = "7a3c8d33ecbdacfe36f0127c1056cce24a29172c"')
    return s
edit("backend/Cargo.toml", cargo)

def domain(s):
    s = replace_once(s, 'request_model: "gpt-6.1-sol".into(),',
                     'request_model: "gpt-6-sol".into(),')
    s = replace_once(s, 'reasoning_effort: "low".into(),',
                     'reasoning_effort: "none".into(),')
    s = replace_once(s, 'confirmation_rounds: 1,', 'confirmation_rounds: 3,')
    begin = s.index("    pub fn classify(&mut self, score: &Score) -> &'static str {")
    end = s.index("\n    }\n}", begin) + len("\n    }")
    replacement = """    pub fn classify(&mut self, score: &Score) -> &'static str {
        let requested = self.config.canonical(&self.config.request_model);
        // A closed-set scorer CANNOT authenticate a request model absent from the bank.
        // This also protects existing stored configurations from being reinterpreted.
        if !score.results.iter().any(|candidate| candidate.model == requested) {
            self.consecutive = 0;
            return "inconclusive";
        }
        if score.probability * 100.0 < self.config.low_confidence_percent {
            self.consecutive = 0;
            return "low_confidence";
        }
        let cfg = &self.config;
        if !cfg.degraded_models.iter().any(|m| m == &requested)
            && cfg.degraded_models.contains(&score.prediction)
            && score.probability * 100.0 >= cfg.degraded_percent
        {
            self.consecutive += 1;
            if self.consecutive >= cfg.confirmation_rounds {
                "degraded"
            } else {
                "suspected"
            }
        } else {
            self.consecutive = 0;
            // A confident *different* unknown candidate is not "not degraded".
            if score.prediction == requested
                && score.probability * 100.0 >= cfg.degraded_percent {
                "no_match"
            } else {
                "inconclusive"
            }
        }
    }"""
    s = s[:begin] + replacement + s[end:]
    # Update the existing unit-test helpers to enumerate enrolled models.
    s = replace_once(s,
        'results: vec![],\n            families: serde_json::json!([]),',
        '''results: vec![
                Candidate { model: "gpt-6-sol".into(), label: "GPT 6 Sol".into(),
                    family: "gpt".into(), probability: 0.0 },
                Candidate { model: "gpt-5.6-luna".into(), label: "GPT 5.6 Luna".into(),
                    family: "gpt".into(), probability: p },
            ],
            families: serde_json::json!([]),''')
    # The conservative default requires 3 consecutive confirmed probe rounds.
    s = replace_once(s,
        'assert_eq!(j.classify(&score("gpt-5.6-luna", 0.90)), "degraded");',
        'assert_eq!(j.classify(&score("gpt-5.6-luna", 0.90)), "suspected");')
    # Existing alias exemption test is still valid because both model IDs are enrolled.
    return s
edit("backend/src/domain.rs", domain)

edit("backend/src/manager.rs", lambda s: replace_once(
    s,
    '"no_match" => j.finish("completed", "no_match", "未降智", now()),',
    '"no_match" => j.finish("completed", "no_match", "库内未发现配置的降级模型特征；并非型号认证", now()),'
))

p = repo / "plugin.json"
manifest = json.loads(p.read_text(encoding="utf-8"))
assert manifest["version"] == "1.14.0"
assert manifest["engines"]["codex-proxy-rs"] == ">=3.38.1, <3.39.0"
assert manifest["resources"]["runtime/runtime.tar.gz"] == "application/gzip"
manifest["version"] = "1.15.0"
manifest["engines"]["codex-proxy-rs"] = ">=3.40.0, <3.41.0"
manifest["description"] = "原生 Rust ModelTrace 账户指纹检测、定时检查和指纹库在线更新，无 Python/NumPy 运行时"
manifest["resources"].pop("runtime/runtime.tar.gz")
# All bundled runtime library notices go away. Keep the upstream project notice.
manifest["resources"].pop("legal/THIRD_PARTY.txt", None)
p.write_text(json.dumps(manifest, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")

# Do not remove author-source Python; GitHub CI uses the original as a
# numerical reference, but it is no longer a package resource.
assert not any(key.startswith("runtime/") for key in manifest["resources"])
assert (repo / "vendor/modeltrace/data/unified_bank.json").is_file()
print("JZJC 1.15.0 native scorer prepared; no Python runtime packaged")
