"""django-pitchfork: Rust-powered search core for django-haystack.

Evolution path: Strategy C (standalone Rust search server + Haystack backend
adapter) now, Strategy B (in-process PyO3 embed) next.
"""

__version__ = "0.1.0"

default_app_config = None  # no Django app config needed; this is a backend package
