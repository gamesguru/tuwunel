#!/bin/bash
set -eo pipefail

track_name="Complement Cryptography"
results="tests/complement-crypto/results.jsonl"

# shellcheck source=./summarise_complement.sh
. "$(dirname "$0")/summarise_complement.sh"

summarise_main "$@"
