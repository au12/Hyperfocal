import sys

from .server import healthcheck, main

if len(sys.argv) > 1 and sys.argv[1] == "healthcheck":
    sys.exit(healthcheck())
sys.exit(main())
