#!/usr/bin/env python
"""Derive the Python support matrix from pyproject.toml classifiers.

Bolt's check_support_matrix.py pattern: CI matrices are derived, never
hardcoded, so versions cannot drift.
"""

import sys
from pathlib import Path

try:
    import tomllib
except ImportError:  # Python < 3.11
    tomllib = None
    import tomli as tomllib  # type: ignore[no-redef]

MIN_PYTHON = (3, 10)
MIN_DJANGO = (4, 2)


def main() -> int:
    pyproject = Path(__file__).parent.parent / "pyproject.toml"
    data = tomllib.loads(pyproject.read_text())
    classifiers = data.get("project", {}).get("classifiers", [])

    pythons = sorted(
        tuple(int(part) for part in c.split(" ")[-1].split(".")[:2])
        for c in classifiers
        if c.startswith("Programming Language :: Python :: 3.")
    )
    djangos = sorted(
        tuple(int(part) for part in c.split(" :: ")[-1].split(".")[:2])
        for c in classifiers
        if c.startswith("Framework :: Django :: ")
    )

    if not pythons or not djangos:
        print("ERROR: no Python/Django classifiers found in pyproject.toml")
        return 1

    if pythons[0] < MIN_PYTHON:
        print(f"ERROR: Python floor {pythons[0]} is below {MIN_PYTHON}")
        return 1
    if djangos[0] < MIN_DJANGO:
        print(f"ERROR: Django floor {djangos[0]} is below {MIN_DJANGO}")
        return 1

    print(f"python-versions: {', '.join('.'.join(map(str, v)) for v in pythons)}")
    print(f"django-versions: {', '.join('.'.join(map(str, v)) for v in djangos)}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
