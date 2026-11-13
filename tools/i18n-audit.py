"""Which interface strings are missing from the catalogs, and where.

Two different problems are reported separately:

1. a `tr("...")` key that is absent from a catalog, so the locale falls back to
   English;
2. a UI literal that never goes through `tr()` at all, so it can never be
   translated.
"""
import json
import re
import sys
from collections import defaultdict
from pathlib import Path

root = Path(__file__).resolve().parent.parent
locales = root / "locales"


def catalog(name):
    return json.loads((locales / name).read_text(encoding="utf-8"))["strings"]


ru = catalog("ru.json")
zh = catalog("zh.json")
en = catalog("en.json")

KEY = re.compile(r'\btr\(\s*"((?:[^"\\]|\\.)*)"\s*\)')
TRF = re.compile(r'\btrf\(\s*"((?:[^"\\]|\\.)*)"\s*,')

used = defaultdict(set)
for path in sorted((root / "src").rglob("*.rs")):
    text = path.read_text(encoding="utf-8")
    for match in KEY.finditer(text):
        # The source spells newlines as escapes; the runtime key has real ones.
        used[match.group(1).replace("\\n", "\n")].add(path.relative_to(root).as_posix())
    for match in TRF.finditer(text):
        used[match.group(1).replace("\\n", "\n")].add(path.relative_to(root).as_posix())

missing_ru = {k: v for k, v in used.items() if k not in ru}
missing_zh = {k: v for k, v in used.items() if k not in zh}

print(f"tr() keys in code: {len(used)}")
print(f"en.json: {len(en)} strings")
print(f"ru.json: {len(ru)} strings, missing {len(missing_ru)}")
print(f"zh.json: {len(zh)} strings, missing {len(missing_zh)}")

print("\n== keys missing from ru.json ==")
by_file = defaultdict(list)
for key, files in missing_ru.items():
    for f in files:
        by_file[f].append(key)
for f in sorted(by_file):
    print(f"\n-- {f} ({len(by_file[f])})")
    for key in sorted(by_file[f]):
        print(f"   {key}")

print("\n== keys missing from zh.json ==")
print(f"   {len(missing_zh)} keys")

# Literals handed to egui without tr(): a string argument that is plain text.
LITERAL = re.compile(
    r'\.(?:label|text|heading|checkbox|selectable_value|radio_value|button|tooltip_text)\('
    r'[^,\n]*?,\s*"((?:[^"\\]|\\.){3,})"'
)
print("\n== literals that are not wrapped in tr() ==")
found = 0
for path in sorted((root / "src").rglob("*.rs")):
    text = path.read_text(encoding="utf-8")
    for match in LITERAL.finditer(text):
        line = text[: match.start()].count("\n") + 1
        value = match.group(1)
        if value.startswith("{") or value.isdigit():
            continue
        print(f"   {path.relative_to(root).as_posix()}:{line}: {value}")
        found += 1
print(f"   total: {found}")
