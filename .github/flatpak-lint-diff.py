"""Check flatpak-builder-lint output against the errors this project accepts.

The linter encodes Flathub's submission policy rather than whether the app
works, and a few of its rules land on choices made deliberately. Those are
listed in flatpak-lint-allowed.json with the reason each one stands. Anything
else fails the build.

An accepted error that stopped being reported fails too. Otherwise the list
grows into a set of claims nobody has rechecked, which is how a real regression
ends up hiding behind an entry that was written for something else.
"""

import json
import sys
from pathlib import Path
from typing import cast

USAGE = "usage: flatpak-lint-diff.py <allowed.json> <report.json>..."

JsonObject = dict[str, object]


def load_object(path: str) -> JsonObject:
    """The JSON object in `path`, empty for an empty file or a non-object.

    `json.loads` is typed `Any`. Parsing goes through here so that ends in one
    place and nothing downstream inherits it.
    """
    text = Path(path).read_text().strip()
    if not text:
        return {}
    # The stdlib types json.loads as Any, so this is where the untyped world
    # ends. Annotating the target stops it spreading, and the isinstance below
    # is what earns the cast.
    parsed: object = json.loads(text)  # pyright: ignore[reportAny]
    return cast(JsonObject, parsed) if isinstance(parsed, dict) else {}


def errors_in(path: str) -> set[str]:
    """The error ids in one lint report. A report with none is written as an
    empty file or as JSON without the key; both read as nothing reported."""
    errors = load_object(path).get("errors")
    if not isinstance(errors, list):
        return set()
    return {str(error) for error in cast(list[object], errors)}


def accepted(path: str) -> set[str]:
    """The error ids this project accepts. The reason each one stands is the
    value beside it, kept for whoever reads the file rather than for this."""
    return set(load_object(path))


def main(argv: list[str]) -> int:
    if len(argv) < 3:
        print(USAGE, file=sys.stderr)
        return 2

    allowed_path = argv[1]
    allowed = accepted(allowed_path)
    reported: set[str] = set()
    for path in argv[2:]:
        reported |= errors_in(path)

    unexpected = sorted(reported - allowed)
    stale = sorted(allowed - reported)

    for error in unexpected:
        print(f"::error::flatpak-builder-lint reports {error}")
    for error in stale:
        print(f"::error::{error} is no longer reported, drop it from {allowed_path}")
    if unexpected:
        print(f"Fix it, or add it to {allowed_path} with the reason it stands.")

    return 1 if unexpected or stale else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
