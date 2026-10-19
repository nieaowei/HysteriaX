# Template validator compatibility fixtures

Frozen inputs and expected verdicts/normalized JSON-pointer paths from jsonschema 0.33.0, retained as the compatibility contract for the production validator. These are synthetic configurations with placeholder credentials.

Run `python3 scripts/verify-subscription-validator-compatibility.py` from the repository root. The generators require PyYAML and produce 1,036 main cases plus 10 edge cases in a temporary directory. Expected results must be reviewed when intentionally changing schema behavior; do not regenerate them automatically from the current validator.
