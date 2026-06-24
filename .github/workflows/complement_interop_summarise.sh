#!/bin/bash
set -eo pipefail

track_name="Complement Interoperability"
jsonl="tests/complement/interop/results.jsonl"
#metrics_tar="tests/complement/interop/runtime_metrics.tar.zst"

# Diff against the homogeneous Complement board so known tuwunel failures render
# orange (a shared gap) rather than red (an interop regression), and render the
# grid over that board's full test list so the two line up square-for-square.
# logs_jsonl drives the peer-homeserver detection that blacks out non-federation
# tests.
baseline_jsonl="tests/complement/results.jsonl"
logs_jsonl="tests/complement/interop/logs.jsonl"
render_over_baseline=1

# Mark progressed cells with a green crossed box to set the interop board apart
# from the homogeneous boards' green check.
adv_cell="❎"

# shellcheck source=./complement_summarise.sh
. "$(dirname "$0")/complement_summarise.sh"

summarise_main "$@"
