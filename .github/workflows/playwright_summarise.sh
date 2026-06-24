#!/bin/bash
set -eo pipefail

json="tests/playwright/results.json"
acceptlist="tests/playwright/known-failures.txt"
out="${GITHUB_STEP_SUMMARY:-/dev/stdout}"

# Spec tree → one row per "file :: title", classified by worst project status.
# Tab-delimited because ids contain spaces.
classify() {
	jq -r '
		[.. | objects | select(has("tests") and has("title") and has("file"))]
		| map({
			id: "\(.file) :: \(.title)",
			s:  ([.tests[].status]
			     | if   any(. == "unexpected") then "error"
			       elif any(. == "flaky")      then "flaky"
			       elif any(. == "expected")   then "accept"
			       else                             "skip"
			       end)
		  })
		| unique_by(.id) | sort_by(.id)[]
		| "\(.s)\t\(.id)"
	' "$1"
}

snapshot_current() {
	classify "$json" > "$curr";
}

snapshot_baseline() {
	git show "HEAD:$json" 2>/dev/null | classify /dev/stdin > "$prev" || :;
}

count_status() {
	grep -cP "^$1\t" "$curr" || :;
}

count_lines() {
	printf '%s' "$1" | grep -c . || :;
}

# Rows of $1 whose status matches any "|"-separated token in $2, sorted by id.
filter_status() {
	awk -F'\t' -v p="$2" '
		BEGIN { n = split(p, a, "|"); for (i = 1; i <= n; i++) want[a[i]] = 1 }
		want[$1]
	' "$1" | sort -t $'\t' -k2,2
}

# HEAD-delta: ids whose status flipped from any-of-$1 (prev) to any-of-$2 (curr).
delta() {
	join -t $'\t' -j2 <(filter_status "$prev" "$1") <(filter_status "$curr" "$2") | cut -f1
}

# Current errors not on the known-failures acceptlist.
gate_violators() {
	awk -F'\t' '$1=="error" {print $2}' "$curr" | sort -u \
		| comm -23 - <(sort -u "$acceptlist" 2>/dev/null) | grep -v '^$' || :
}

grid_width() {
	awk -v n="$1" 'BEGIN { w=int(sqrt(n)); if (w*w<n) w++; print (w?w:1) }';
}

# Cells: ✅/🟩 accept, 🟨 flaky, 🟥/🟧 error, ⬛ skip, ⬜ grid filler. Acceptlisted
# errors are 🟧 (expected); off-list errors and acceptlisted regressions are 🟥.
render_grid() {
	awk -F'\t' -v w="$1" -v R="$2" -v P="$3" -v A="$4" '
		BEGIN {
			split(R, x, "\n"); for (i in x) if (x[i]) reg[x[i]] = 1
			split(P, x, "\n"); for (i in x) if (x[i]) pro[x[i]] = 1
			split(A, x, "\n"); for (i in x) if (x[i]) acc[x[i]] = 1
		}
		{
			t = $2
			if      ($1 == "skip")   c = "⬛"
			else if ($1 == "flaky")  c = "🟨"
			else if ($1 == "accept") c = pro[t]              ? "✅" : "🟩"
			else if ($1 == "error")  c = (reg[t] || !acc[t]) ? "🟥" : "🟧"
			printf "%s", c
			if (++n % w == 0) printf "<br>"
		}
		END {
			while (n < w*w) { printf "⬜"; if (++n % w == 0) printf "<br>" }
		}
	' "$curr"
}

emit_header() {
	echo "### Playwright"
	echo
	echo "| accept | errors | flakes | skipped | advanced | regressed |"
	echo "|---|---|---|---|---|---|"
	echo "| $accept | $error | $flaky | $skip | $nprog | $nreg |"
	if test -n "$1"; then
		echo
		echo "$1"
	fi
}

# Diff block: 🟨 gate violators, 🟥 regress, 🟩 progress. Gate violators and
# regress share the `-` (red bg) prefix; emoji disambiguates. A regress that
# is also a gate violator surfaces only as 🟨 (the severer signal).
emit_diff() {
	local regress_only="$regress"
	if test -n "$gate"; then
		regress_only=$(printf '%s\n' "$regress" | grep -vxFf <(printf '%s\n' "$gate") || :)
	fi

	test -n "$gate$regress_only$progress" || return 0
	echo
	echo '```diff'
	printf '%s\n' "$gate"         | sed -n 's/^./- &/p'
	printf '%s\n' "$regress_only" | sed -n 's/^./- &/p'
	printf '%s\n' "$progress"     | sed -n 's/^./+ &/p'
	echo '```'
}

main() {
	if test ! -s "$json"; then
		echo "No results.json produced." >> "$out"
		exit 1
	fi

	curr=$(mktemp); prev=$(mktemp)
	trap 'rm -f "$curr" "$prev"' EXIT
	snapshot_current
	snapshot_baseline

	accept=$(count_status accept)
	error=$( count_status error)
	flaky=$( count_status flaky)
	skip=$(  count_status skip)
	total=$((accept + error + flaky + skip))

	regress=$( delta "accept|flaky" "error")
	progress=$(delta "error"        "accept|flaky")
	nreg=$( count_lines "$regress")
	nprog=$(count_lines "$progress")

	nobase=0
	test -s "$prev" || { regress= progress= nobase=1; }

	gate=$(gate_violators)
	ngate=$(count_lines "$gate")

	# Main-branch runs are the baseline; the grid carries no diff signal there.
	if test "${GITHUB_REF_NAME:-}" = "main"; then
		grid=
	else
		acclist=$(cat "$acceptlist" 2>/dev/null || true)
		grid=$(render_grid "$(grid_width "$total")" "$regress" "$progress" "$acclist")
	fi

	{ emit_header "$grid"; emit_diff; } >> "$out"

	test "$ngate" -eq 0
}

# Per-shard gate: fail this shard on any error not on the acceptlist. The verdict
# rides on the execution job, so a rerun re-executes the tests, not a stale summary.
gate_main() {
	if test ! -s "$json"; then
		echo "No results.json produced." >> "$out"
		exit 1
	fi

	curr=$(mktemp)
	trap 'rm -f "$curr"' EXIT
	snapshot_current

	gate=$(gate_violators)
	ngate=$(count_lines "$gate")

	if test "$ngate" -ne 0; then
		{
			echo "### Playwright Shard"
			echo
			echo '```diff'
			printf '%s\n' "$gate" | sed -n 's/^./- &/p'
			echo '```'
		} >> "$out"
	fi

	test "$ngate" -eq 0
}

case "${1:-}" in
	gate) gate_main ;;
	*)    main "$@" ;;
esac
