#!/bin/bash
set -eo pipefail

track_name="Complement-Crypto"
jsonl="tests/complement-crypto/results.jsonl"

# shellcheck source=./complement_summarise_lib.sh
. "$(dirname "$0")/complement_summarise_lib.sh"

summarise_main "$@"
