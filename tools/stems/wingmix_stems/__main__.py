"""`python -m wingmix_stems [--fetch-weights MODEL]`; see protocol.py."""

import sys

from .protocol import main

sys.exit(main(sys.argv[1:]))
