#!/bin/bash
set -eo pipefail

BASEDIR=$(dirname "$0")

default_cargo_profile="test"
default_feat_set="all"
default_rust_toolchain="nightly"
default_rust_target="x86_64-unknown-linux-gnu"
default_sys_name="debian"
default_sys_target="x86_64-v1-linux-gnu"
default_sys_version="testing-slim"

default_complement_verbose=0
default_complement_dirty=0
default_complement_count=1
default_complement_parallel=1
default_complement_shuffle=0
default_complement_timeout="1h"
default_complement_run=".*"

run="${1:-$default_complement_run}"

skip=""
skip="${skip}TestThreadReceiptsInSyncMSC4102"
skip="${skip}|TestToDeviceMessagesOverFederation/stopped_server"
skip="${skip}|TestRestrictedRoomsRemoteJoinFailOver"
skip="${skip}|TestRestrictedRoomsRemoteJoinFailOverInMSC3787Room"
skip="${skip}|TestToDeviceMessagesOverFederation/interrupted_connectivity"

set -a
cargo_profile="${cargo_profile:-$default_cargo_profile}"
feat_set="${feat_set:-$default_feat_set}"
rust_target="${rust_target:-$default_rust_target}"
rust_toolchain="${rust_toolchain:-$default_rust_toolchain}"
sys_name="${sys_name:-$default_sys_name}"
sys_target="${sys_target:-$default_sys_target}"
sys_version="${sys_version:-$default_sys_version}"

runner_name=$(echo $RUNNER_NAME | cut -d"." -f1)
runner_num=$(echo $RUNNER_NAME | cut -d"." -f2)
set +a

envs=""
envs="$envs -e complement_verbose=${complement_verbose:-$default_complement_verbose}"
envs="$envs -e complement_count=${complement_count:-$default_complement_count}"
envs="$envs -e complement_dirty=${complement_dirty:-$default_complement_dirty}"
envs="$envs -e complement_parallel=${complement_parallel:-$default_complement_parallel}"
envs="$envs -e complement_shuffle=${complement_shuffle:-$default_complement_shuffle}"
envs="$envs -e complement_timeout=${complement_timeout:-$default_complement_timeout}"
envs="$envs -e complement_skip=${complement_skip:-$skip}"
envs="$envs -e complement_run=${1:-$default_complement_run}"
envs="$envs -e COMPLEMENT_ALWAYS_PRINT_SERVER_LOGS=1"
envs="$envs -e COMPLEMENT_DESTROY_HS_TIMEOUT_SECS=10"

flavor="complement"
tester_image_prefix="complement-tester"
container_name_prefix="complement_tester"
src_root="/usr/src/complement"
results_dir="tests/complement"
export flavor tester_image_prefix container_name_prefix src_root results_dir
export envs run

exec "$BASEDIR/complement-runner.sh"
