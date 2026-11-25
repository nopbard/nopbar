"""Audit clippy diagnostic source spans against the capture/reminder base commit."""
import json
import subprocess
import argparse
from functools import lru_cache
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
parser = argparse.ArgumentParser()
parser.add_argument("--base", default="b734afb7")
parser.add_argument("--input", default="target/capture-clippy-final.jsonl")
args = parser.parse_args()
BASE = args.base


@lru_cache(None)
def original(name):
    result = subprocess.run(["git", "show", f"{BASE}:{name}"], cwd=ROOT, capture_output=True)
    return result.stdout.decode("utf-8", errors="replace") if result.returncode == 0 else ""


seen, changed = set(), []
for line in (ROOT / args.input).read_text(encoding="utf-8").splitlines():
    entry = json.loads(line)
    if entry.get("reason") != "compiler-message":
        continue
    message = entry["message"]
    if message["level"] not in ("warning", "error"):
        continue
    spans = [s for s in message["spans"] if s["is_primary"]]
    key = (message["message"], tuple((s["file_name"], s["line_start"]) for s in spans))
    if key in seen:
        continue
    seen.add(key)
    for span in spans:
        name = span["file_name"].replace("\\", "/")
        if name.startswith("/rustc/"):
            continue
        before = original(name)
        snippets = [s["text"][s["highlight_start"] - 1:s["highlight_end"] - 1].strip() for s in span["text"]]
        if message["level"] == "error" or not all(s in before for s in snippets):
            changed.append((name, span["line_start"], message["message"]))

print(f"Unique diagnostics: {len(seen)}; changed/new source diagnostics: {len(changed)}")
for item in changed:
    print(item)
raise SystemExit(bool(changed))
