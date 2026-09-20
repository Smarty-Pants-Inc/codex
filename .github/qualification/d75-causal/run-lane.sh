#!/usr/bin/env bash
# One outer timeout owns all native prerequisite, compile and named-test phases.
set -euo pipefail
: "${D75_RECIPE:?}" "${D75_LANE:?}"
exec python3 "$D75_RECIPE/run-lane.py" "$D75_LANE"
