"""Test runner for django-pitchfork, mirroring django-haystack's run_tests.py.

Sets DJANGO_SETTINGS_MODULE=pitchfork_tests.settings and puts both repos on
sys.path (django-haystack provides the test models/fixtures).
"""

import multiprocessing
import os
import sys
from argparse import ArgumentParser

REPO_ROOT = os.path.dirname(os.path.abspath(__file__))
PROJECT_ROOT = os.path.dirname(REPO_ROOT)

# django-haystack checkout (sibling of this repo by default).
HAYSTACK_ROOT = os.environ.get(
    "HAYSTACK_ROOT", os.path.join(os.path.dirname(PROJECT_ROOT), "django-haystack")
)


def make_parser():
    parser = ArgumentParser(description="Run the django-pitchfork test suite.")
    parser.add_argument(
        "labels",
        nargs="*",
        default=["pitchfork_tests"],
        help="Django test labels (default: the whole pitchfork_tests suite)",
    )
    return parser


def main():
    parser = make_parser()
    args = parser.parse_args()

    # Python >= 3.14 defaults to forkserver; haystack's state inheritance
    # requires fork (same workaround as haystack's run_tests.py).
    if sys.version_info >= (3, 14):
        multiprocessing.set_start_method("fork")

    sys.path.insert(0, PROJECT_ROOT)
    sys.path.insert(0, os.path.join(PROJECT_ROOT, "python"))
    if os.path.isdir(HAYSTACK_ROOT):
        sys.path.insert(0, HAYSTACK_ROOT)
    else:
        print(f"warning: django-haystack not found at {HAYSTACK_ROOT}", file=sys.stderr)

    os.environ["DJANGO_SETTINGS_MODULE"] = "pitchfork_tests.settings"

    import django
    from django.test.utils import get_runner
    from django.conf import settings

    django.setup()

    test_runner = get_runner(settings)(verbosity=1, interactive=True)
    failures = test_runner.run_tests(args.labels)
    sys.exit(bool(failures))


if __name__ == "__main__":
    main()
