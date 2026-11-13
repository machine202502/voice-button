"""Add interface strings to the three desktop catalogs.

Reads a JSON file of the form

    {"English key": {"ru": "перевод", "zh": "翻译"}}

and appends every key that is not there yet to `locales/en.json`,
`locales/ru.json` and `locales/zh.json`. The text is inserted just before the
closing brace of the `strings` object, so existing entries keep their order and
formatting.
"""
import json
import sys
from pathlib import Path

root = Path(__file__).resolve().parent.parent
additions = json.loads(Path(sys.argv[1]).read_text(encoding="utf-8-sig"))


def escape(text):
    return json.dumps(text, ensure_ascii=False)


def add(path, pick):
    text = path.read_text(encoding="utf-8")
    catalog = json.loads(text)["strings"]
    block = []
    for key, entry in additions.items():
        if key in catalog:
            continue
        value = pick(key, entry)
        block.append(f"    {escape(key)}: {escape(value)}")
    if not block:
        return 0
    marker = text.rstrip().rfind("}")           # end of the file
    marker = text.rstrip()[:marker].rstrip().rfind("}")  # end of "strings"
    head = text[:marker].rstrip()
    tail = text[marker:]
    if not head.endswith("{"):
        head += ","
    path.write_text(head + "\n" + ",\n".join(block) + "\n  " + tail.lstrip(), encoding="utf-8")
    return len(block)


added = {}
added["en"] = add(root / "locales" / "en.json", lambda key, entry: key)
added["ru"] = add(root / "locales" / "ru.json", lambda key, entry: entry.get("ru", key))
added["zh"] = add(root / "locales" / "zh.json", lambda key, entry: entry.get("zh", key))

for code, count in added.items():
    print(f"{code}: +{count}")
