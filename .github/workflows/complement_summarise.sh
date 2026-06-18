#!/bin/bash
set -eo pipefail

track_name="Complement"
jsonl="tests/complement/results.jsonl"
metrics_tar="tests/complement/runtime_metrics.tar.zst"

# shellcheck source=./complement_summarise_lib.sh
. "$(dirname "$0")/complement_summarise_lib.sh"

summarise_main "$@"
