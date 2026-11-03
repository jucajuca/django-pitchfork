"""Exceptions for django-pitchfork."""


class PitchforkError(Exception):
    """Base error for django-pitchfork."""


class PitchforkServerError(PitchforkError):
    """The pitchfork-server returned an error response."""


class PitchforkWireError(PitchforkError):
    """Wire protocol violation (version mismatch, bad payload)."""
