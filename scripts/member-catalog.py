#!/usr/bin/env python3
"""Generate editor method signatures from the canonical specification."""
import json
from pathlib import Path
import re
import sys

root = Path(__file__).resolve().parent.parent
groups = {}
sections = {"13.3": "string", "14.1": "list", "14.2": "map", "14.3": "set", "15.1": "json", "15.2": "time"}
for name in ["13-stdlib-core.md", "14-stdlib-collections.md", "15-stdlib-data.md"]:
    group = None
    for line in (root / "docs/spec" / name).read_text().splitlines():
        heading = re.match(r"## (\d+\.\d+) ", line)
        if heading:
            group = sections.get(heading[1])
        if group and line.startswith("| `"):
            for signature in re.findall(r"`([^`]+)`", line.split(" | ")[0]):
                match = re.match(r"(?:(?:json|time)\.)?(\w+)(?:<[^>]+>)?\(", signature)
                if match and match[1] not in {"mapFrom", "set"}:
                    groups.setdefault((group, match[1]), []).append(signature)
text = (root / "docs/spec/08-errors-and-results.md").read_text().split("## 8.4 Result methods")[1].split("```")[1]
for signature in text.strip().splitlines():
    groups[("result", re.match(r"\w+", signature)[0])] = [signature]
output = "// Signatures from docs/spec. Rebuild with scripts/member-catalog.py.\n"
output += "pub(crate) const METHODS: &[(&str, &str, &str)] = &[\n"
for (group, name), signatures in sorted(groups.items()):
    output += "    (" + ", ".join(json.dumps(s) for s in (group, name, " | ".join(signatures))) + "),\n"
output += "];\n"
target = root / "crates/hollowscript-ide/src/member_catalog.rs"
if "--check" in sys.argv:
    if target.read_text() != output:
        sys.exit("member catalogue is stale: run python3 scripts/member-catalog.py")
else:
    target.write_text(output)
