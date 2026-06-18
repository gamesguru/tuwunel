#!/bin/bash

set +e

builder="${GITHUB_ACTOR}"
seed_builder="${seed_builder:-jevolk}"

# Commit-message (or workflow_dispatch) directives controlling the per-actor
# buildx builder, so the runner cache can be reset without an ssh trip:
#   [ci clean]          discard the builder so it is recreated from scratch,
#                       picking up the current nightly toolchain and a fresh
#                       buildkit.
#   [ci clean nocache]  ...and recreate cold, skipping the seed-from-seed_builder
#                       step below.
#   [ci clean-rust]     refresh the rust toolchain. A rust-and-above prune that
#                       keeps the base system is not matchable by buildx, so for
#                       now this settles for the same cold rebuild as nocache.
clean=
nocache=
case "$pipeline" in
*"[ci clean nocache]"*) clean=1; nocache=1 ;;
*"[ci clean-rust]"*)    clean=1; nocache=1 ;;
*"[ci clean]"*)         clean=1 ;;
esac

if test -n "$clean"; then
	docker buildx rm "$builder"
fi

docker buildx inspect "$builder"
if test x"$?" = x"0"; then
	exit 0
fi

set -eux

reserved_space=$(echo -n "$reserved_space" | jq -r --arg r "$runner" '.[$r] // .["aws"]')
max_used_space=$(echo -n "$max_used_space" | jq -r --arg r "$runner" '.[$r] // .["aws"]')
min_free_space=$(echo -n "$min_free_space" | jq -r --arg r "$runner" '.[$r] // .["aws"]')
safety_free_space=$(echo -n "$safety_free_space" | jq -r --arg r "$runner" '.[$r] // .["aws"]')
trunk_max_used=$(echo -n "$trunk_max_used" | jq -r --arg r "$runner" '.[$r] // .["aws"]')
branch_max_used=$(echo -n "$branch_max_used" | jq -r --arg r "$runner" '.[$r] // .["aws"]')
unlabeled_max_used=$(echo -n "$unlabeled_max_used" | jq -r --arg r "$runner" '.[$r] // .["aws"]')
leaf_max_used=$(echo -n "$leaf_max_used" | jq -r --arg r "$runner" '.[$r] // .["aws"]')
cachemount_max_used=$(echo -n "$cachemount_max_used" | jq -r --arg r "$runner" '.[$r] // .["aws"]')

cat <<EOF > ./buildkitd.toml
[system]
  platformsCacheMaxAge = "504h"
[worker.oci]
  enabled = true
  rootless = false
  gc = true
  reservedSpace = "${reserved_space}"
  maxUsedSpace = "${max_used_space}"
  minFreeSpace = "${min_free_space}"

# Leaf: per-run outputs (build-bins, build-tests, install, complement,
# integ, smoke, packaging) plus the final image targets (static,
# docker, oci, complement-testee) we want to reuse across runs.
# Sized to hold ~1-2 full default-matrix runs of leaf images.
[[worker.oci.gcpolicy]]
  filters = ["label==cache.tier==leaf"]
  keepDuration = "1h"
  maxUsedSpace = "${leaf_max_used}"
  all = true

# Cache mounts: cargo registry, cargo git, rustup downloads. Slow-
# changing, expensive to refetch; given their own bucket so the
# leaf-tier churn doesn't evict them.
[[worker.oci.gcpolicy]]
  filters = ["type==cachemount"]
  keepDuration = "168h"
  maxUsedSpace = "${cachemount_max_used}"
  all = true

# Unlabeled records: intermediate exec layers, frontend bookkeeping,
# anything not explicitly tiered. Excludes cachemount via type filter
# so the cachemount bucket above is the sole evictor for those.
[[worker.oci.gcpolicy]]
  filters = ["label!=cache.tier==leaf,label!=cache.tier==branch,label!=cache.tier==trunk,type!=cachemount"]
  keepDuration = "24h"
  maxUsedSpace = "${unlabeled_max_used}"
  all = true

# Branch: cooked deps (deps-base), rocksdb compile, cargo. Expensive
# cargo_rust_feat_sys nodes. Day-scale age protection; size cap is
# the dominant trigger so small runners can churn this tier normally.
[[worker.oci.gcpolicy]]
  filters = ["label==cache.tier==branch"]
  keepDuration = "24h"
  maxUsedSpace = "${branch_max_used}"
  all = true

# Trunk: foundation layers (system, rust). Week-scale age protection
# so trunk survives any normal pressure; size cap is the backstop.
[[worker.oci.gcpolicy]]
  filters = ["label==cache.tier==trunk"]
  keepDuration = "168h"
  maxUsedSpace = "${trunk_max_used}"
  all = true

# Safety floor: under critical disk pressure, evict anything regardless
# of tier or age. Only relief valve when tier caps haven't sufficed.
[[worker.oci.gcpolicy]]
  minFreeSpace = "${safety_free_space}"
  all = true
EOF

# Seed a brand-new builder from seed_builder's cache so it starts warm instead
# of from scratch; buildkit reuses the layers that match and rebuilds the rest
# per the new actor's needs. The buildkit state lives in a docker volume named
# for the builder, so the seed is a volume copy done before bootstrap. When the
# seed builder is absent (its state volume does not exist), or for the seed
# builder itself, [ci clean nocache], and [ci clean-rust], this is skipped and
# the builder is cold-created.
seed_state="buildx_buildkit_${seed_builder}0_state"
this_state="buildx_buildkit_${builder}0_state"
seeded=
if test -z "$nocache" \
	&& test "$builder" != "$seed_builder" \
	&& docker volume inspect "$seed_state" >/dev/null 2>&1
then
	docker volume create "$this_state"
	if docker run --rm \
		-v "${seed_state}:/seed:ro" \
		-v "${this_state}:/state" \
		busybox sh -c 'cp -a /seed/. /state/'
	then
		seeded=1
	else
		docker volume rm -f "$this_state"
	fi
fi

create_builder() {
	docker buildx create \
		--bootstrap \
		--driver docker-container \
		--buildkitd-config ./buildkitd.toml \
		--name "$builder" \
		--buildkitd-flags "--allow-insecure-entitlement network.host"
}

# A seed copied from a live builder can carry a torn cache.db; if bootstrap
# rejects it, discard the seed and cold-start so a build is never blocked.
if ! create_builder; then
	if test -n "$seeded"; then
		docker buildx rm "$builder" || true
		docker volume rm -f "$this_state" || true
		create_builder
	else
		exit 1
	fi
fi
