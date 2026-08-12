"""Entry point for the setup wizard.

This exists so that the wizard is *imported* as part of the `wizard` package
rather than run as a loose script. Freezing `wizard\\wizard.py` directly makes
it `__main__` with no parent package, and every `from . import ...` inside it
fails at startup with "attempted relative import with no known parent
package" -- which is a thing that only shows up in the packaged build, because
running it any other way goes through the package properly.

    python vbnote_setup.py            the wizard
    python -m wizard --eboot ...      the same work with no window
"""
import sys

from wizard.wizard import main

if __name__ == "__main__":
    sys.exit(main())
