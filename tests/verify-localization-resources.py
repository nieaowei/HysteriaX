"""Check translation coverage and placeholder parity without translating API identifiers."""
import json
import re
from pathlib import Path

root = Path(__file__).resolve().parents[1] / "apps/macos/Sources/HysteriaX"
string = r'"(?:[^"\\]|\\.)*"'

def read_table(language):
    content = (root / f"Resources/{language}.lproj/Localizable.strings").read_text()
    entries = re.findall(rf"^({string}) = ({string});$", content, re.MULTILINE)
    table = {json.loads(key): json.loads(value) for key, value in entries}
    assert len(table) == len(entries), f"Duplicate keys in {language}"
    return table

english, chinese = read_table("en"), read_table("zh-Hans")
assert english.keys() == chinese.keys(), "Language tables have different keys"
for key, value in english.items():
    assert sorted(re.findall(r"\{\d+\}", value)) == sorted(re.findall(r"\{\d+\}", chinese[key])), key
    assert not re.search(r"[\u4e00-\u9fff]", value), f"Untranslated English value: {key}"
for file in root.rglob("*.swift"):
    source = file.read_text()
    error_table = re.search(r"private static let errors = \[(.*?)\n    \]", source, re.DOTALL)
    if error_table:
        for _, value in re.findall(rf"({string}): ({string})", error_table.group(1)):
            assert json.loads(value) in english, f"Missing error translation in {file}: {value}"
    for key in re.findall(rf"L10n.text\(({string})", source):
        assert json.loads(key) in english, f"Missing key in {file}: {key}"
print(f"Verified {len(english)} bilingual entries and all source references.")
