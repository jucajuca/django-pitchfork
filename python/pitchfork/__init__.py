"""pitchfork: Rust-powered search engine for django-haystack.

Both modes are shipped: server mode (a standalone `pitchfork-server` process)
and embedded mode (the same Rust core in-process via the `pitchfork._core`
PyO3 extension). `pitchfork.backends.embedded.AutoEngine` picks between them.

The haystack-replacement API is a separate, future package: `django-pitchfork`.
"""

__version__ = "0.1.0"

default_app_config = None  # no Django app config needed; this is a backend package
