#!/bin/bash
# Shared helpers for the per-flavour complement summariser drivers. Sourced;
# not invoked directly. Drivers set:
#
#   track_name      Heading text (e.g. "Complement", "Complement-Crypto").
#   jsonl           Path to the per-flavour results.jsonl.
#   metrics_tar     Optional. Path to per-run runtime_metrics.tar.zst. When
#                   set, emit_runtime_metrics() will be wired into main.
#
# Runtime-metrics history is read from and written to small JSON digests the
# workflow restores from / saves to the GitHub Actions cache, passed via env:
#   RUNTIME_DIGEST_OWN   this branch's rolling ring (read for history columns,
#                        rewritten for the green-gated save step).
#   RUNTIME_DIGEST_MAIN  the default branch's anchor (read only; unset on it).
#   RUNTIME_DIGEST_KEEP  ring depth (3 off the default branch, 1 on it).
#
# Drivers then call `summarise_main "$@"`.

set -eo pipefail

out="${GITHUB_STEP_SUMMARY:-/dev/stdout}"

# Some tests emit both pass and fail rows (subtests). Fold by Test, fail wins.
classify() {
	jq -sr '
		group_by(.Test)[]
		| { t: .[0].Test, s:
		      (if   any(.[]; .Action == "fail") then "error"
		       elif any(.[]; .Action == "pass") then "accept"
		       else                                  "skip"
		       end) }
		| "\(.s) \(.t)"
	' "$1"
}

snapshot_current() {
	classify "$jsonl" | sort -k2 > "$curr";
}

snapshot_baseline() {
	git show "HEAD:${baseline_jsonl:-$jsonl}" 2>/dev/null | classify /dev/stdin | sort -k2 > "$prev" || :;
}

# Interop only: the top-level tests that deployed a peer homeserver this run,
# i.e. the tests an image override can actually affect. A peer's server name
# (hs2, hs3, ...) appears in the captured output only when that server was
# deployed; tests with only hs1 never mention it and become black board filler.
# Pre-filter the (large) log with grep so jq parses only the matching lines.
affected_tests() {
	test -s "${logs_jsonl:-}" || return 0
	grep -aE 'hs[2-9]' "$logs_jsonl" | jq -r '.Test' 2>/dev/null | sort -u
}

# Count test names in a newline-joined list, split by section (no '/') vs
# subtest (the "X/Y" rows Go emits beneath a top-level test).
count_sec() {
	printf '%s' "$1" | grep -c '^[^/]*$' || :;
}

count_sub() {
	printf '%s' "$1" | grep -c '/' || :;
}

delta() {
	join -j2 -t ' ' \
		<(awk -v k="$1" '$1==k' "$prev") \
		<(awk -v k="$2" '$1==k' "$curr") \
		| cut -d' ' -f1
}

# Fixed column count for every flavour's grid, anchored to the Complement
# track's square width (ceil(sqrt(791)) = 29 at time of writing). Holding the
# width constant lets the smaller Complement-Crypto suite render as a short
# horizontal band instead of a tiny square; grid height then floats with each
# flavour's test count. A driver may override by setting grid_width first.
grid_width="${grid_width:-29}"

# Cells: ✅/🟩 for accept (progressed vs stable; the interop board passes ❎ as
# $adv_cell for progressed), 🟥/🟧 for error (regressed vs known), ⬛ a skipped
# test or, on the interop board, a slot with no effective result (did not run,
# or deploys no peer homeserver and so is inapplicable), ⬜ grid filler past the
# last test. Squares are emitted over $universe (the full board test list);
# statuses are read from $curr (the effective result rows). For non-interop
# flavours $universe is $curr, so the only blank slot is trailing filler. With
# no baseline (nobase=1) all fails are red and all passes plain green.
render_grid() {
	awk -v w="$1" -v nobase="$2" -v R="$3" -v P="$4" -v adv="${adv_cell:-✅}" '
		BEGIN {
			split(R, x, "\n"); for (i in x) if (x[i]) reg[x[i]] = 1
			split(P, x, "\n"); for (i in x) if (x[i]) pro[x[i]] = 1
		}
		NR == FNR { st[$2] = $1; next }
		{
			t = $2
			if      (!(t in st) || st[t] == "skip") c = "⬛"
			else if (st[t] == "accept")             c = pro[t]             ? adv : "🟩"
			else                                    c = (nobase || reg[t]) ? "🟥" : "🟧"
			printf "%s", c
			if (++n % w == 0) printf "<br>"
		}
		END {
			while (n % w != 0) { printf "⬜"; if (++n % w == 0) printf "<br>" }
		}
	' "$curr" "$universe"
}

# Passing rate: accept over the row's test count (accept + errors + skips).
# Skips stay in the denominator, so a flaky skip holds the rate below 100%
# until it is fixed. Empty rows render "n/a" rather than dividing by zero.
pct() {
	awk -v a="$1" -v d="$2" 'BEGIN {
		if (d == 0) print "n/a"; else printf "%.1f%%\n", 100 * a / d
	}'
}

# Emit one markdown table row from its cells.
row() { printf '| %s | %s | %s | %s | %s | %s | %s |\n' "$@"; }

emit_header() {
	local pass_sec pass_sub pass_tot
	pass_sec=$(pct "$acc_sec" "$((acc_sec + err_sec + skip_sec))")
	pass_sub=$(pct "$acc_sub" "$((acc_sub + err_sub + skip_sub))")
	pass_tot=$(pct "$acc_tot" "$((acc_tot + err_tot + skip_tot))")

	echo "### $track_name"
	echo
	echo "|  | accept | errors | skipped | advanced | regressed | passing |"
	echo "|---|---|---|---|---|---|---|"
	row sections "$acc_sec" "$err_sec" "$skip_sec" "$nprog_sec" "$nreg_sec" "$pass_sec"
	row subtests "$acc_sub" "$err_sub" "$skip_sub" "$nprog_sub" "$nreg_sub" "$pass_sub"
	row total "$acc_tot" "$err_tot" "$skip_tot" "$nprog_tot" "$nreg_tot" "$pass_tot"
	if test -n "$1"; then
		echo
		echo "$1"
	fi
}

emit_diff() {
	test -n "$regress$progress" || return 0
	echo
	echo '```diff'
	printf '%s\n' "$regress"  | sed -n 's/^./- &/p'
	printf '%s\n' "$progress" | sed -n 's/^./+ &/p'
	echo '```'
}

# Render the runtime-metrics table. History columns come from the cache
# digests the workflow already restored (this branch's ring and the default
# branch's anchor); the updated ring is written back to RUNTIME_DIGEST_OWN for
# the workflow's green-gated save step. Soft-fails: no metrics tar, no python,
# no digests all degrade to the current-run column alone.
emit_runtime_metrics() {
	# A failed run's timings are unreliable, so neither record nor display them.
	test "${execute_outcome:-success}" = "success" || return 0
	test -n "${metrics_tar:-}" || return 0
	test -s "$metrics_tar" || return 0
	command -v python3 >/dev/null 2>&1 || return 0

	local script="$(dirname "$BASH_SOURCE")/complement_metrics_summarise.py"
	test -x "$script" || return 0

	local args=(--tar "$metrics_tar" --out "$out")
	if test -n "${RUNTIME_DIGEST_OWN:-}"; then
		test -s "$RUNTIME_DIGEST_OWN" && args+=(--history-in "$RUNTIME_DIGEST_OWN")
		args+=(--history-out "$RUNTIME_DIGEST_OWN" --keep "${RUNTIME_DIGEST_KEEP:-3}")
	fi
	test -s "${RUNTIME_DIGEST_MAIN:-}" && args+=(--main-in "$RUNTIME_DIGEST_MAIN")
	python3 "$script" "${args[@]}" || :
}

summarise_main() {
	if test ! -s "$jsonl"; then
		echo "No results.jsonl produced." >> "$out"
		exit 0
	fi

	curr=$(mktemp); prev=$(mktemp); leaves=$(mktemp)
	trap 'rm -f "$curr" "$prev" "$leaves"' EXIT
	snapshot_current
	snapshot_baseline

	# Interop: keep only tests that deployed a peer homeserver; the rest are
	# inert under the image override and become black board filler below. Diff
	# and tallies then speak only to tests that actually exercised interop.
	if test -n "${logs_jsonl:-}"; then
		local aff; aff=$(mktemp)
		affected_tests > "$aff"
		if test -s "$aff"; then
			awk 'NR == FNR { a[$1] = 1; next }
			     { t = $2; sub(/\/.*/, "", t); if (t in a) print }' \
				"$aff" "$curr" > "$curr.eff" && mv "$curr.eff" "$curr"
		fi
		rm -f "$aff"
	fi

	regress=$( delta accept error)
	progress=$(delta error  accept)

	nobase=0
	test -s "$prev" || { regress= progress= nobase=1; }

	# Tallies split by section (top-level test) vs subtest. The accept/error/
	# skip counts come from the classified snapshot; advanced/regressed are the
	# diff lists partitioned by the same '/' discriminator.
	read -r acc_sec err_sec skip_sec acc_sub err_sub skip_sub < <(awk '
		{ is_sub = ($2 ~ /\//) }
		$1 == "accept" { if (is_sub) sub_a++; else sec_a++ }
		$1 == "error"  { if (is_sub) sub_e++; else sec_e++ }
		$1 == "skip"   { if (is_sub) sub_k++; else sec_k++ }
		END { printf "%d %d %d %d %d %d\n",
		             sec_a + 0, sec_e + 0, sec_k + 0,
		             sub_a + 0, sub_e + 0, sub_k + 0 }
	' "$curr")

	nprog_sec=$(count_sec "$progress"); nprog_sub=$(count_sub "$progress")
	nreg_sec=$( count_sec "$regress");  nreg_sub=$( count_sub "$regress")

	# Distinct leaf tests for the total row. A classified row is a leaf when no
	# other row nests beneath it ("<name>/..."); a parent only aggregates its
	# subtests (fail wins above), so counting parents alongside their leaves
	# double-counts. Go nests arbitrarily deep, so leaf-ness is computed over
	# every ancestor prefix, not just the top level. The total thus need not
	# equal sections + subtests.
	awk '
		{ nm[NR] = $2; m = split($2, p, "/"); pre = p[1]
		  for (i = 2; i <= m; i++) { internal[pre] = 1; pre = pre "/" p[i] } }
		END { for (i = 1; i <= NR; i++) if (!(nm[i] in internal)) print nm[i] }
	' "$curr" | sort > "$leaves"

	read -r acc_tot err_tot skip_tot < <(awk '
		NR == FNR { leaf[$0] = 1; next }
		$2 in leaf { if ($1 == "accept") a++; else if ($1 == "error") e++; else k++ }
		END { printf "%d %d %d\n", a + 0, e + 0, k + 0 }
	' "$leaves" "$curr")

	nprog_tot=$(printf '%s' "$progress" | grep -Fxcf "$leaves" || :)
	nreg_tot=$( printf '%s' "$regress"  | grep -Fxcf "$leaves" || :)

	# Squares are emitted over $universe. Interop renders over the homogeneous
	# baseline list so the board lines up square-for-square with the Complement
	# board; every other flavour renders over its own (effective) result rows.
	if test -n "${render_over_baseline:-}" && test -s "$prev"; then
		universe="$prev"
	else
		universe="$curr"
	fi

	# Main-branch runs are the baseline; the grid carries no diff signal there.
	if test "${GITHUB_REF_NAME:-}" = "main"; then
		grid=
	else
		grid=$(render_grid "$grid_width" "$nobase" "$regress" "$progress")
	fi

	{ emit_header "$grid"; emit_diff; } >> "$out"
	emit_runtime_metrics
}
