"""Test settings module wiring a PitchforkEngine connection.

Mirrors django-haystack's test_haystack/settings.py but points
HAYSTACK_CONNECTIONS at the pitchfork backend over HTTP.
"""

import os
from tempfile import mkdtemp

SECRET_KEY = "Please do not spew DeprecationWarnings"

DATABASES = {
    "default": {"ENGINE": "django.db.backends.sqlite3", "NAME": ":memory:"}
}

DEFAULT_AUTO_FIELD = "django.db.models.BigAutoField"

INSTALLED_APPS = [
    "django.contrib.admin",
    "django.contrib.auth",
    "django.contrib.contenttypes",
    "django.contrib.messages",
    "django.contrib.sessions",
    "haystack",
    "test_haystack.core",
]

TEMPLATES = [
    {
        "BACKEND": "django.template.backends.django.DjangoTemplates",
        "APP_DIRS": True,
        "OPTIONS": {
            "context_processors": [
                "django.template.context_processors.request",
                "django.contrib.auth.context_processors.auth",
                "django.contrib.messages.context_processors.messages",
            ]
        },
    }
]

MIDDLEWARE = [
    "django.contrib.sessions.middleware.SessionMiddleware",
    "django.contrib.auth.middleware.AuthenticationMiddleware",
    "django.contrib.messages.middleware.MessageMiddleware",
]

ROOT_URLCONF = "test_haystack.core.urls"

HAYSTACK_ROUTERS = ["haystack.routers.DefaultRouter"]

# The server port is shared with pitchfork_tests/testcases.py (both read the
# same env var) so the fixture subprocess listens where the backend connects.
TEST_PITCHFORK_PORT = os.environ.get("TEST_PITCHFORK_PORT", "8117")
TEST_PITCHFORK_URL = f"http://127.0.0.1:{TEST_PITCHFORK_PORT}/"
TEST_PITCHFORK_DATA = os.environ.get(
    "TEST_PITCHFORK_DATA", mkdtemp(prefix="test_pitchfork_data")
)

HAYSTACK_CONNECTIONS = {
    "default": {
        "ENGINE": "pitchfork.backends.server.PitchforkEngine",
        "URL": TEST_PITCHFORK_URL,
        "TIMEOUT": 30,
    },
}

# Embedded mode (Strategy B) is selected via the PITCHFORK_MODE env var:
# "embedded" runs the same suite against the in-process Rust module. No
# PATH is configured: each setup() gets a fresh data dir so the swapped
# unified indexes in the test classes rebuild cleanly.
if os.environ.get("PITCHFORK_MODE") == "embedded":
    HAYSTACK_CONNECTIONS["default"] = {
        "ENGINE": "pitchfork.backends.embedded.PitchforkEmbeddedEngine",
    }

USE_TZ = False
