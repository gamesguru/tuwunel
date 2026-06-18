#!/usr/bin/env -S python3 -S
"""Aggregate Tuwunel runtime-metrics dumps from a Complement run."""

import argparse, json, os, re, subprocess, sys, tarfile, time
from collections import defaultdict
from pathlib import Path

###############################################################################
# Debug-string parsing
#
# tokio-metrics RuntimeMetrics and nix Usage emit Rust Debug strings in the
# JSON "payload" field because they do not implement Serialize.

DURATION_RE = re.compile(r"(\d+(?:\.\d+)?)(ns|µs|us|ms|s)$")
UNIT_NS     = {"ns": 1, "µs": 1_000, "us": 1_000, "ms": 1_000_000, "s": 1_000_000_000}

RT_INTS = (
	("workers_count",             "workers_count"),
	("total_polls_count",         "total_polls"),
	("total_park_count",          "total_park"),
	("total_steal_count",         "total_steal"),
	("num_remote_schedules",      "remote_schedules"),
	("total_local_schedule_count","local_schedules"),
	("total_overflow_count",      "overflow_count"),
	("budget_forced_yield_count", "budget_yields"),
	("io_driver_ready_count",     "io_driver_ready"),
	("blocking_threads_count",    "blocking_threads"),
	("live_tasks_count",          "live_tasks"),
)
RT_DURS = (
	("total_busy_duration",  "total_busy_ns"),
	("elapsed",              "elapsed_ns"),
	("mean_poll_duration",   "mean_poll_ns"),
	("max_busy_duration",    "max_busy_ns"),
)
USAGE_INTS = (
	("ru_maxrss",   "maxrss_kb"),
	("ru_minflt",   "minflt"),
	("ru_majflt",   "majflt"),
	("ru_inblock",  "inblock"),
	("ru_oublock",  "oublock"),
	("ru_nvcsw",    "nvcsw"),
	("ru_nivcsw",   "nivcsw"),
	("ru_nsignals", "nsignals"),
)

def parse_dur(s):
	m = DURATION_RE.fullmatch(s.strip())
	return int(float(m.group(1)) * UNIT_NS[m.group(2)]) if m else None

def parse_int(s):
	try: return int(s)
	except ValueError: return None

def parse_timeval(body, label):
	m = re.search(rf"{label}: timeval \{{ tv_sec: (-?\d+), tv_usec: (-?\d+) \}}", body)
	return int(m.group(1)) * 1_000_000 + int(m.group(2)) if m else None

def split_kv(text):
	# Flat 'k: v, k: v' parse, treating {}/[]/() as opaque sub-spans.
	out, key, val, in_key, depth = {}, [], [], True, 0
	for ch in text + ",":
		if in_key:
			if ch == ":" and depth == 0: in_key, val = False, []
			else: key.append(ch)
		elif ch in "{[(": depth += 1; val.append(ch)
		elif ch in "}])": depth -= 1; val.append(ch)
		elif ch == "," and depth == 0:
			out["".join(key).strip()] = "".join(val).strip()
			key, val, in_key = [], [], True
		else: val.append(ch)
	return out

def extract_ints(kv, mapping):
	return {dst: v for src, dst in mapping if src in kv for v in [parse_int(kv[src])] if v is not None}

def extract_durs(kv, mapping):
	return {dst: v for src, dst in mapping if src in kv for v in [parse_dur(kv[src])] if v is not None}

def parse_runtime_metrics(payload):
	m = re.fullmatch(r"\s*RuntimeMetrics \{(.*)\}\s*", payload, re.DOTALL)
	if not m: return {}
	kv = split_kv(m.group(1))
	return {**extract_ints(kv, RT_INTS), **extract_durs(kv, RT_DURS)}

def parse_usage(payload):
	m = re.fullmatch(r"\s*Usage\(rusage \{(.*)\}\)\s*", payload, re.DOTALL)
	if not m: return {}
	body = m.group(1)
	out = {}
	for label, dst in (("ru_utime","utime_us"), ("ru_stime","stime_us")):
		v = parse_timeval(body, label)
		if v is not None: out[dst] = v
	body = re.sub(r"ru_[us]time: timeval \{[^}]*\},?\s*", "", body)
	out.update(extract_ints(split_kv(body), USAGE_INTS))
	return out

###############################################################################
# Tarball ingestion
#
# Layout: runtime_metrics/by_test/<TestName>/<container_id>/
#             tuwunel.runtime_metrics.<pid>.json
#             tuwunel.runtime_usage.<pid>.json

def open_tar(path):
	# stdlib tarfile lacks zstd until 3.14; pipe through the system tool.
	proc = subprocess.Popen(
		["zstd", "-dc", "--", str(path)],
		stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
	)
	return tarfile.open(fileobj=proc.stdout, mode="r|"), proc

def is_dump(member):
	return member.isfile() and member.name.endswith(".json")

def parse_member_path(name):
	# Returns (test, cid, fname) or None for paths outside by_test/.
	parts = name.strip("/").split("/")
	if "by_test" not in parts: return None
	i = parts.index("by_test")
	return tuple(parts[i+1:i+4]) if len(parts) >= i + 4 else None

def read_dump(tar, member):
	fp = tar.extractfile(member)
	if fp is None: return None
	try: return json.loads(fp.read().decode("utf-8"))
	except (UnicodeDecodeError, json.JSONDecodeError): return None

def merge_dump(row, fname, dump):
	payload = dump.get("payload", "")
	if "runtime_metrics" in fname: row.update(parse_runtime_metrics(payload))
	elif "runtime_usage" in fname: row.update(parse_usage(payload))

def load_tar(path):
	rows, version = defaultdict(dict), None
	tar, proc = open_tar(path)
	with tar:
		for m in tar:
			if not is_dump(m): continue
			parsed = parse_member_path(m.name)
			if parsed is None: continue
			test, cid, fname = parsed
			dump = read_dump(tar, m)
			if dump is None: continue
			version = version or dump.get("meta", {}).get("tuwunel_version")
			row = rows[(test, cid)]
			row["test"], row["cid"] = test, cid
			merge_dump(row, fname, dump)
	proc.wait()
	return list(rows.values()), version

###############################################################################
# Reduction

def col(rows, key):
	return [r[key] for r in rows if r.get(key) is not None]

def busy_ratio(r):
	b, e, w = r.get("total_busy_ns"), r.get("elapsed_ns"), max(1, r.get("workers_count", 1))
	return b / e / w if b is not None and e else None

def cpu_total_us(r):
	u, s = r.get("utime_us"), r.get("stime_us")
	return u + s if u is not None and s is not None else None

def derive_col(rows, fn):
	return [v for v in (fn(r) for r in rows) if v is not None]

def pct(xs, q):
	if not xs: return None
	s = sorted(xs)
	if len(s) == 1: return s[0]
	pos = (len(s) - 1) * (q / 100)
	lo, hi = int(pos), min(int(pos) + 1, len(s) - 1)
	return s[lo] + (s[hi] - s[lo]) * (pos - lo)

def safe_max(xs): return max(xs) if xs else None
def safe_sum(xs): return sum(xs) if xs else 0

def aggregate(rows):
	if not rows: return {"testees": 0}
	maxrss = col(rows, "maxrss_kb")
	cpu = derive_col(rows, cpu_total_us)
	busy = derive_col(rows, busy_ratio)
	poll = col(rows, "mean_poll_ns")
	polls = col(rows, "total_polls")
	nvcsw = col(rows, "nvcsw")
	nivcsw = col(rows, "nivcsw")
	inblk = col(rows, "inblock")
	oublk = col(rows, "oublock")
	majflt = col(rows, "majflt")
	minflt = col(rows, "minflt")
	overflow = col(rows, "overflow_count")
	return {
		"testees": len(rows),
		"tests": len({r["test"] for r in rows}),
		"workers_count": rows[0].get("workers_count"),
		"maxrss_kb_median": pct(maxrss, 50),
		"maxrss_kb_p95": pct(maxrss, 95),
		"maxrss_kb_max": safe_max(maxrss),
		"cpu_total_us_median": pct(cpu, 50),
		"cpu_total_us_sum": safe_sum(cpu),
		"busy_ratio_median": pct(busy, 50),
		"busy_ratio_p95": pct(busy, 95),
		"mean_poll_ns_median": pct(poll, 50),
		"mean_poll_ns_p95": pct(poll, 95),
		"polls_median": pct(polls, 50),
		"nvcsw_median": pct(nvcsw, 50),
		"nivcsw_median": pct(nivcsw, 50),
		"inblock_total": safe_sum(inblk),
		"oublock_total": safe_sum(oublk),
		"majflt_total": safe_sum(majflt),
		"minflt_median": pct(minflt, 50),
		"overflow_total": safe_sum(overflow),
	}

###############################################################################
# Formatters

def fmt_kb(kb):
	if kb is None: return "n/a"
	mib = kb / 1024.0
	return f"{mib/1024:.2f} GiB" if mib >= 1024 else f"{mib:.0f} MiB"

def fmt_us(us):
	if us is None: return "n/a"
	if us >= 1_000_000: return f"{us/1_000_000:.2f} s"
	if us >= 1_000: return f"{us/1_000:.1f} ms"
	return f"{us:.0f} µs"

def fmt_ns(ns):
	if ns is None: return "n/a"
	if ns >= 1_000_000_000: return f"{ns/1_000_000_000:.2f} s"
	if ns >= 1_000_000: return f"{ns/1_000_000:.2f} ms"
	if ns >= 1_000: return f"{ns/1_000:.1f} µs"
	return f"{ns:.0f} ns"

def fmt_ratio(r): return "n/a" if r is None else f"{r:.3f}"
def fmt_int(n): return "n/a" if n is None else (f"{n:,.0f}" if isinstance(n, float) else f"{n:,}")

###############################################################################
# Rendering
#
# direction: True  = lower-is-better (RSS, CPU, page faults, ...)
#            False = higher-is-better (busy ratio)
#            None  = neither, no regress/improve marker
# floor:     absolute delta below which we suppress the percent and print "·".

ROWS = (
	("maxrss_kb_median",    "Peak RSS (median)",            fmt_kb,    True,  1024),
	("maxrss_kb_p95",       "Peak RSS (p95)",               fmt_kb,    True,  1024),
	("maxrss_kb_max",       "Peak RSS (max)",               fmt_kb,    True,  1024),
	("cpu_total_us_median", "CPU per testee (median)",      fmt_us,    True,  1_000),
	("cpu_total_us_sum",    "CPU total",                    fmt_us,    True,  1_000_000),
	("busy_ratio_median",   "Runtime busy ratio (median)",  fmt_ratio, False, 0.01),
	("busy_ratio_p95",      "Runtime busy ratio (p95)",     fmt_ratio, False, 0.01),
	("mean_poll_ns_median", "Mean poll duration (median)",  fmt_ns,    True,  1_000),
	("mean_poll_ns_p95",    "Mean poll duration (p95)",     fmt_ns,    True,  1_000),
	("polls_median",        "Polls per testee (median)",    fmt_int,   None,  1),
	("nvcsw_median",        "Voluntary ctxsw (median)",     fmt_int,   None,  1),
	("nivcsw_median",       "Involuntary ctxsw (median)",   fmt_int,   True,  1),
	("inblock_total",       "Block reads (total)",          fmt_int,   True,  1),
	("oublock_total",       "Block writes (total)",         fmt_int,   True,  1),
	("majflt_total",        "Major page faults (total)",    fmt_int,   True,  1),
	("minflt_median",       "Minor page faults (median)",   fmt_int,   None,  1),
	("overflow_total",      "Worker overflow (total)",      fmt_int,   True,  1),
)

def is_regression(direction, delta):
	if direction is None: return None
	return delta > 0 if direction else delta < 0

def fmt_delta(curr, prev, direction, floor):
	if curr is None or prev is None: return ""
	d = curr - prev
	if abs(d) < floor: return "·"
	pct_d = (d / prev * 100.0) if prev else float("inf")
	regress = is_regression(direction, d)
	marker = ""
	if regress is True and abs(pct_d) >= 5: marker = " ⚠️"
	if regress is False and abs(pct_d) >= 5: marker = " ✅"
	sign = "+" if d >= 0 else ""
	return f"{sign}{pct_d:.1f}%{marker}" if prev else f"{sign}{d:.0f}{marker}"

def header_line(curr, version):
	bits = []
	if version: bits.append(f"tuwunel {version}")
	if curr.get("workers_count"): bits.append(f"{curr['workers_count']}-worker tokio runtime")
	bits.append(f"{curr.get('testees', 0)} testees across {curr.get('tests', 0)} tests")
	return ", ".join(bits) + "."

# Column model. Each column is (header, kind, agg):
#   "this"  this run's value;  "delta"  signed % vs the column's agg (markers);
#   "value" a prior run's raw value (no delta).

def short_label(entry):
	sha = (entry.get("sha") or "")[:7]
	return sha or f"run {entry.get('run_id', '?')}"

def build_columns(curr, main_entry, history):
	cols = [("This run", "this", curr)]
	if main_entry is not None:
		cols.append(("Δ vs main", "delta", main_entry["agg"]))
		cols.append((f"main<br>`{short_label(main_entry)}`", "value", main_entry["agg"]))
	for i, e in enumerate(history, start=1):
		branch = e.get("branch") or "prev"
		cols.append((f"{branch} −{i}<br>`{short_label(e)}`", "value", e["agg"]))
	return cols

def cell(curr, col, key, fmt, direction, floor):
	_, kind, agg = col
	if kind == "this":
		v = curr.get(key); return fmt(v) if v is not None else "n/a"
	if kind == "delta":
		return fmt_delta(curr.get(key), agg.get(key), direction, floor)
	v = agg.get(key); return fmt(v) if v is not None else "n/a"

def render_table(curr, cols):
	heads = " | ".join(c[0] for c in cols)
	lines = ["| Metric | " + heads + " |", "|" + "---|" * (len(cols) + 1)]
	for key, lbl, fmt, direction, floor in ROWS:
		cells = " | ".join(cell(curr, c, key, fmt, direction, floor) for c in cols)
		lines.append(f"| {lbl} | " + cells + " |")
	return lines

def hot_rss(rows, n=5):
	xs = [r for r in rows if r.get("maxrss_kb") is not None]
	xs.sort(key=lambda r: (-r["maxrss_kb"], r["test"]))
	return [(r["test"], fmt_kb(r["maxrss_kb"])) for r in xs[:n]]

def hot_cpu(rows, n=5):
	xs = [(r["test"], cpu_total_us(r)) for r in rows if cpu_total_us(r) is not None]
	xs.sort(key=lambda p: (-p[1], p[0]))
	return [(t, fmt_us(c)) for t, c in xs[:n]]

def render_hot(title, items):
	if not items: return []
	out = ["", title, ""]
	for name, value in items: out.append(f"- `{name}`: {value}")
	return out

def render(curr, cols, rows, version):
	out = ["", "#### Runtime metrics", "", header_line(curr, version), ""]
	out += render_table(curr, cols)
	out += render_hot("Top 5 testees by peak RSS:", hot_rss(rows))
	out += render_hot("Top 5 testees by CPU:", hot_cpu(rows))
	if len(cols) == 1:
		out += ["", "_No baseline yet; history warms up over the next few green runs._"]
	out.append("")
	return "\n".join(out)

###############################################################################
# History ring buffer
#
# A small JSON document persisted per (branch, matrix slot) in the GitHub
# Actions cache. Each green run prepends its aggregate and truncates to --keep;
# reads tolerate a cold (absent) or corrupt cache.

BUFFER_VERSION = 1

def load_buffer(path):
	if not path or not Path(path).is_file(): return None
	try: buf = json.loads(Path(path).read_text(encoding="utf-8"))
	except (OSError, json.JSONDecodeError): return None
	return buf if isinstance(buf, dict) else None

def buffer_runs(buf):
	runs = buf.get("runs") if buf else None
	return runs if isinstance(runs, list) else []

def this_entry(agg):
	env = os.environ
	return {
		"run_id":      env.get("GITHUB_RUN_ID", ""),
		"run_attempt": env.get("GITHUB_RUN_ATTEMPT", ""),
		"sha":         env.get("GITHUB_SHA", ""),
		"branch":      env.get("GITHUB_REF_NAME", ""),
		"ts":          int(time.time()),
		"agg":         agg,
	}

def rotate(prev, entry, keep):
	return {"v": BUFFER_VERSION, "runs": ([entry] + buffer_runs(prev))[:max(1, keep)]}

def write_buffer(path, buf):
	p = Path(path)
	p.parent.mkdir(parents=True, exist_ok=True)
	p.write_text(json.dumps(buf), encoding="utf-8")

###############################################################################
# Entry point

def usable(path):
	return path and Path(path).is_file() and Path(path).stat().st_size > 0

def parse_args():
	ap = argparse.ArgumentParser()
	ap.add_argument("--tar", required=True)
	ap.add_argument("--history-in")
	ap.add_argument("--main-in")
	ap.add_argument("--history-out")
	ap.add_argument("--keep", type=int, default=3)
	ap.add_argument("--out", default="-")
	ap.add_argument("--emit-json")
	return ap.parse_args()

def write_out(path, body):
	if path == "-": sys.stdout.write(body)
	else: open(path, "a", encoding="utf-8").write(body)

def main():
	args = parse_args()
	if not usable(args.tar): return 0
	rows, version = load_tar(args.tar)
	curr = aggregate(rows)

	prev_own = load_buffer(args.history_in)
	history = buffer_runs(prev_own)[:max(0, args.keep)]
	main_runs = buffer_runs(load_buffer(args.main_in))
	main_entry = main_runs[0] if main_runs else None
	cols = build_columns(curr, main_entry, history)

	if args.emit_json: Path(args.emit_json).write_text(json.dumps(curr, indent=2))
	if args.history_out: write_buffer(args.history_out, rotate(prev_own, this_entry(curr), args.keep))
	write_out(args.out, render(curr, cols, rows, version))
	return 0

if __name__ == "__main__":
	sys.exit(main())
