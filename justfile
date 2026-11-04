# pitchfork task runner (bolt's justfile pattern: single entry point
# bridging cargo + Python).
default:
    @just --list

# Build the Rust crates, server binary and the PyO3 extension module
# (required after any Rust change).
build:
    cargo build
    -@just _maturin

# Build the PyO3 extension into the current venv (best-effort: server mode
# works without it).
_maturin:
    .venv/bin/maturin develop

# Native Rust tests (core + engine + server).
test-rs:
    cargo test

# Python backend test suite: server mode (live fixture) and embedded mode
# (in-process Rust module) — same suite, two modes.
test-py:
    .venv/bin/python pitchfork_tests/run_tests.py
    PITCHFORK_MODE=embedded .venv/bin/python pitchfork_tests/run_tests.py

# Everything.
test: test-rs test-py

# Lint gate: clippy with -D warnings before tests (haystack's ruff-first
# pattern, in Rust).
clippy:
    cargo clippy --all-targets -- -D warnings

# Coverage (Rust).
coverage:
    cargo llvm-cov --summary-only

# Weekly fuzz workflow entry point; locally: extended Hypothesis cases.
fuzz:
    DJANGO_SETTINGS_MODULE=pitchfork_tests.settings \
      .venv/bin/python -m pytest pitchfork_tests/test_fuzz.py -q \
      -p no:cacheprovider
    @echo "Extended proptest cases + mutation testing run in CI; see .github/workflows/fuzz.yml"

# Record a benchmark artifact (BENCHMARK.md numbers).
save-bench:
    @echo "Run benchmarks and record numbers in BENCHMARK.md"

# Release: bump all version declarations together, tag, push. The package
# version is dynamic (read from the root Cargo.toml by maturin); bump it in
# the workspace block, mirror it in python/pitchfork/__init__.py, and record
# the release in CHANGELOG.md first.
release VERSION:
    #!/bin/sh
    set -eu
    grep -q "version = \"{{ VERSION }}\"" Cargo.toml || { echo "Cargo.toml is not at {{ VERSION }} (workspace version)"; exit 1; }
    grep -q "__version__ = \"{{ VERSION }}\"" python/pitchfork/__init__.py || { echo "python/pitchfork/__init__.py is not at {{ VERSION }}"; exit 1; }
    grep -q "## \[{{ VERSION }}\]" CHANGELOG.md || { echo "CHANGELOG.md has no {{ VERSION }} entry"; exit 1; }
    git add Cargo.toml Cargo.lock python/pitchfork/__init__.py CHANGELOG.md
    git commit -m "release {{ VERSION }}" || true
    git tag "v{{ VERSION }}"
    # Push from the current branch (this repo's default is master).
    git push origin HEAD "v{{ VERSION }}"
