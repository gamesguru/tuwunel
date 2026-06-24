#!/bin/bash
set -eo pipefail

track_name="Complement Cryptography"
jsonl="tests/complement-crypto/results.jsonl"

# shellcheck source=./complement_summarise.sh
. "$(dirname "$0")/complement_summarise.sh"

summarise_main "$@"
