//! Compare MSC4521 strategies without building the homeserver.
//!
//! Run:
//!   cargo run --release --manifest-path benches/Cargo.toml
//!   cargo run --release --manifest-path benches/Cargo.toml -- \
//!       --population 1000000 --rtt-ms 50 --bandwidth-mbps 20
//!
//! The harness reports:
//!   1. root sketch build/query cost for fixed and incremental populations
//!   2. adaptive rounds, upload bytes, and strata estimate per difference size
//!   3. modeled latency with configurable RTT, bandwidth, and measured CPU
//!
//! It does not include the server, RocksDB, HTTP, or event fetching.

use std::{
	collections::HashSet,
	env,
	error::Error,
	hint::black_box,
	time::{Duration, Instant},
};

use rezzy_recon::{
	BucketRequest, ElementHash, FixedWindowElementHash, FixedWindowPopulation, Gf256Decoder,
	IncrementalFixedWindowPopulation, MAX_STRATA_FACTOR_WORK, ResidentKernel, SortedPopulation,
	build_bucket_sketches, build_gf256_bucket_sketches, estimate_strata,
};

const CAP: usize = 32;
const MAX_DEPTH: u8 = 32;
const MAX_ROUNDS: usize = 20;
const STRATA_BYTES: usize = 2_048;
const EVENT_ID_BYTES: usize = 44;

struct Config {
	population: usize,
	deltas: Vec<usize>,
	rtt_ms: f64,
	bandwidth_mbps: f64,
	one_sided: bool,
}

impl Default for Config {
	fn default() -> Self {
		Self {
			population: 100_000,
			deltas: vec![0, 1, 4, 16, 32, 64, 128, 512],
			rtt_ms: 50.0,
			bandwidth_mbps: 20.0,
			one_sided: false,
		}
	}
}

fn main() -> Result<(), Box<dyn Error>> {
	let config = parse_args()?;
	let decoder = Gf256Decoder::cached().ok_or("failed to build the GF(2^256) decoder")?;

	println!(
		"population={} base events, delta {} (local {} / remote {}), cap={}, RTT={}ms, \
		 bandwidth={}Mbps",
		config.population,
		if config.one_sided { "one-sided" } else { "two-sided" },
		if config.one_sided { "all" } else { "half" },
		if config.one_sided { "none" } else { "half" },
		CAP,
		config.rtt_ms,
		config.bandwidth_mbps,
	);
	println!();

	// Table 1: fixed root-sketch cost, and the incremental alternative. Root
	// cost is independent of delta because it touches the whole population.
	let base = (0..config.population)
		.map(|index| digest(index as u64))
		.collect::<Vec<_>>();
	let base64 = SortedPopulation::new(
		base.iter()
			.map(|digest| ElementHash::from_digest32(*digest))
			.collect(),
	);
	let base256 = FixedWindowPopulation::new(
		base.iter()
			.map(|digest| FixedWindowElementHash::from_digest32(*digest))
			.collect(),
	)
	.map_err(|error| format!("{error:?}"))?;

	let (root64_ok, root64_ms) = probe64(&base64, &base64);
	let (root256_ok, root256_ms) = probe256(&base256, &base256, decoder);

	let inc_start = Instant::now();
	let inc256 = IncrementalFixedWindowPopulation::with_default_depth(
		base.iter()
			.map(|digest| FixedWindowElementHash::from_digest32(*digest)),
	)
	.map_err(|error| format!("{error:?}"))?;
	let inc_build_ms = inc_start.elapsed().as_secs_f64() * 1e3;
	let requests = [BucketRequest::new(0, 0, CAP)];
	let inc_root_ms = avg_time(5, || {
		let sketches = inc256
			.bucket_sketches(&requests)
			.expect("query incremental root");
		black_box(sketches);
	});

	println!("root sketch cost for the whole population (both sides build):");
	println!("{:<28} {:>12} {:>12} {:>10}", "profile", "build_ms", "wire", "status");
	println!(
		"{:<28} {:>12} {:>12} {:>10}",
		"GF(2^64) fixed",
		format!("{:.3}", root64_ms.as_secs_f64() * 1e3),
		format_bytes(CAP * 8),
		if root64_ok { "ok" } else { "overflow" }
	);
	println!(
		"{:<28} {:>12} {:>12} {:>10}",
		"GF(2^256) fixed",
		format!("{:.3}", root256_ms.as_secs_f64() * 1e3),
		format_bytes(CAP * 32),
		if root256_ok { "ok" } else { "overflow" }
	);
	println!(
		"{:<28} {:>12} {:>12} {:>10}",
		"GF(2^256) incremental",
		format!("{:.3}", inc_root_ms.as_secs_f64() * 1e3),
		format_bytes(CAP * 32),
		"cached"
	);
	println!(
		"incremental cache build (one-time): {inc_build_ms:.3} ms; depth={}, leaves={}",
		inc256.depth(),
		1_usize << inc256.depth(),
	);
	println!();

	// Table 2: per-delta strategy. Latency uses the measured incremental CPU,
	// not the uncached fixed population.
	println!(
		"{:>6} {:>6} {:>6} {:>9} {:>9} {:>8} {:>9} {:>10} {:>10} {:>8}",
		"delta",
		"r64",
		"r256",
		"k64",
		"k256",
		"est",
		"strata_ms",
		"lat_ad_ms",
		"lat_st_ms",
		"winner"
	);

	for &delta in &config.deltas {
		let (local, remote) = build_digests(&base, delta, config.one_sided);
		let expected256 = symmetric_difference(&local, &remote);
		let expected64: HashSet<u64> = expected256
			.iter()
			.map(|digest| ElementHash::from_digest32(*digest).h64)
			.collect();

		let local256 = FixedWindowPopulation::new(
			local
				.iter()
				.map(|digest| FixedWindowElementHash::from_digest32(*digest))
				.collect(),
		)
		.map_err(|error| format!("{error:?}"))?;
		let remote256 = FixedWindowPopulation::new(
			remote
				.iter()
				.map(|digest| FixedWindowElementHash::from_digest32(*digest))
				.collect(),
		)
		.map_err(|error| format!("{error:?}"))?;
		let local64 = SortedPopulation::new(
			local
				.iter()
				.map(|digest| ElementHash::from_digest32(*digest))
				.collect(),
		);
		let remote64 = SortedPopulation::new(
			remote
				.iter()
				.map(|digest| ElementHash::from_digest32(*digest))
				.collect(),
		);

		let local_kernel = kernel(&local)?;
		let remote_kernel = kernel(&remote)?;

		let adaptive64 = adaptive64(&local64, &remote64);
		let adaptive256 = adaptive256(&local256, &remote256, decoder);

		let strata_ms = avg_time(5, || {
			let estimate = estimate_strata(
				local_kernel.strata(),
				remote_kernel.strata(),
				MAX_STRATA_FACTOR_WORK,
			)
			.expect("strata estimate");
			black_box(estimate);
		});
		let est = estimate_strata(
			local_kernel.strata(),
			remote_kernel.strata(),
			MAX_STRATA_FACTOR_WORK,
		)
		.map_err(|error| format!("{error:?}"))?
		.delta;

		let ok64 = adaptive64.roots == expected64;
		let ok256 = adaptive256.roots == expected256;
		if !ok64 || !ok256 {
			println!("delta={delta}: verification failed (gf64={ok64}, gf256={ok256})");
		}

		let response_bytes = adaptive256.items.saturating_mul(EVENT_ID_BYTES);
		let provisioned = provisioned_bytes(est);
		let cpu_ms = adaptive256.rounds as f64 * inc_root_ms.as_secs_f64() * 1e3;
		let lat_adaptive = model_latency(
			adaptive256.rounds as f64,
			cpu_ms,
			adaptive256.upload_bytes + response_bytes,
			config.rtt_ms,
			config.bandwidth_mbps,
		);
		// One digest/strata preflight plus one provisioned diff round.
		let lat_strata = model_latency(
			2.0,
			inc_root_ms.as_secs_f64() * 1e3,
			STRATA_BYTES + provisioned + response_bytes,
			config.rtt_ms,
			config.bandwidth_mbps,
		);

		println!(
			"{:>6} {:>6} {:>6} {:>9} {:>9} {:>8} {:>9} {:>10.2} {:>10.2} {:>8}",
			delta,
			adaptive64.rounds,
			adaptive256.rounds,
			format_bytes(adaptive64.upload_bytes),
			format_bytes(adaptive256.upload_bytes),
			est,
			format!("{:.3}", strata_ms.as_secs_f64() * 1e3),
			lat_adaptive,
			lat_strata,
			if lat_adaptive <= lat_strata {
				"adaptive"
			} else {
				"strata"
			},
		);
	}

	println!();
	println!(
		"lat_ad_ms: one-shot root then split, {0} RTT per round plus measured incremental CPU \
		 and sketch/response bytes.",
		"rounds"
	);
	println!(
		"lat_st_ms: one digest/strata preflight plus one provisioned diff round, 2048 B strata, \
		 measured incremental CPU, same response estimate."
	);
	println!(
		"Adaptive and strata both need a snapshot; its O(N) cost is excluded. Strata only wins \
		 when adaptive needs 3+ rounds."
	);

	Ok(())
}

fn parse_args() -> Result<Config, Box<dyn Error>> {
	let mut config = Config::default();
	let mut args = env::args().skip(1);
	while let Some(arg) = args.next() {
		match arg.as_str() {
			| "--population" =>
				config.population = args.next().ok_or("missing value")?.parse()?,
			| "--rtt-ms" => config.rtt_ms = args.next().ok_or("missing value")?.parse()?,
			| "--bandwidth-mbps" =>
				config.bandwidth_mbps = args.next().ok_or("missing value")?.parse()?,
			| "--deltas" => {
				config.deltas = args
					.next()
					.ok_or("missing value")?
					.split(',')
					.map(str::parse)
					.collect::<Result<_, _>>()?;
			},
			| "--one-sided" => config.one_sided = true,
			| "--help" | "-h" => {
				println!(
					"usage: benches [--population N] [--deltas a,b,c] [--rtt-ms F] \
					 [--bandwidth-mbps F] [--one-sided]"
				);
				std::process::exit(0);
			},
			| other => return Err(format!("unknown argument: {other}").into()),
		}
	}
	Ok(config)
}

fn build_digests(
	base: &[[u8; 32]],
	delta: usize,
	one_sided: bool,
) -> (Vec<[u8; 32]>, Vec<[u8; 32]>) {
	let mut local = Vec::with_capacity(base.len() + delta);
	let mut remote = Vec::with_capacity(base.len() + delta);
	local.extend_from_slice(base);
	remote.extend_from_slice(base);
	let (local_extra, remote_extra) = if one_sided {
		(delta, 0)
	} else {
		(delta / 2, delta - delta / 2)
	};
	for index in 0..local_extra {
		local.push(digest(1_000_000_000 + index as u64));
	}
	for index in 0..remote_extra {
		remote.push(digest(2_000_000_000 + index as u64));
	}
	(local, remote)
}

fn digest(seed: u64) -> [u8; 32] {
	let mut state = seed
		.wrapping_mul(0x9E37_79B9_7F4A_7C15)
		.wrapping_add(1);
	let mut out = [0_u8; 32];
	for chunk in out.chunks_exact_mut(8) {
		state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
		let mut z = state;
		z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
		z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
		z ^= z >> 31;
		chunk.copy_from_slice(&z.to_le_bytes());
	}
	if out == [0_u8; 32] {
		out[31] = 1;
	}
	out
}

fn kernel(digests: &[[u8; 32]]) -> Result<ResidentKernel, Box<dyn Error>> {
	let mut kernel = ResidentKernel::new();
	for digest in digests {
		kernel
			.insert(FixedWindowElementHash::from_digest32(*digest))
			.map_err(|error| format!("{error:?}"))?;
	}
	Ok(kernel)
}

fn symmetric_difference(local: &[[u8; 32]], remote: &[[u8; 32]]) -> HashSet<[u8; 32]> {
	let local: HashSet<[u8; 32]> = local.iter().copied().collect();
	let remote: HashSet<[u8; 32]> = remote.iter().copied().collect();
	local
		.symmetric_difference(&remote)
		.copied()
		.collect()
}

fn probe64(local: &SortedPopulation, remote: &SortedPopulation) -> (bool, Duration) {
	let requests = [BucketRequest::new(0, 0, CAP)];
	let mut ok = false;
	let elapsed = avg_time(3, || {
		let local_sketch =
			build_bucket_sketches(local.h64s(), &requests).expect("build local GF64 sketch");
		let remote_sketch =
			build_bucket_sketches(remote.h64s(), &requests).expect("build remote GF64 sketch");
		let residual = local_sketch[0]
			.subtract(&remote_sketch[0])
			.expect("subtract GF64 sketches");
		ok = residual.decode_elements(CAP).is_ok();
		black_box(ok);
	});
	(ok, elapsed)
}

fn probe256(
	local: &FixedWindowPopulation,
	remote: &FixedWindowPopulation,
	decoder: &Gf256Decoder,
) -> (bool, Duration) {
	let requests = [BucketRequest::new(0, 0, CAP)];
	let mut ok = false;
	let elapsed = avg_time(3, || {
		let local_sketch =
			build_gf256_bucket_sketches(local, &requests).expect("build local GF256 sketch");
		let remote_sketch =
			build_gf256_bucket_sketches(remote, &requests).expect("build remote GF256 sketch");
		let residual = local_sketch[0]
			.subtract(&remote_sketch[0])
			.expect("subtract GF256 sketches");
		ok = residual.decode_digests(CAP, decoder).is_ok();
		black_box(ok);
	});
	(ok, elapsed)
}

struct Adaptive64 {
	rounds: usize,
	upload_bytes: usize,
	roots: HashSet<u64>,
}

fn adaptive64(local: &SortedPopulation, remote: &SortedPopulation) -> Adaptive64 {
	let mut frontier = vec![BucketRequest::new(0, 0, CAP)];
	let mut rounds = 0;
	let mut upload_bytes = 0;
	let mut roots = HashSet::new();
	for _ in 0..MAX_ROUNDS {
		if frontier.is_empty() {
			break;
		}
		rounds += 1;
		let local_sketches =
			build_bucket_sketches(local.h64s(), &frontier).expect("build local GF64 sketches");
		let remote_sketches =
			build_bucket_sketches(remote.h64s(), &frontier).expect("build remote GF64 sketches");
		upload_bytes += frontier
			.len()
			.saturating_mul(CAP)
			.saturating_mul(8);
		let mut next = Vec::new();
		for ((request, local_sketch), remote_sketch) in frontier
			.iter()
			.zip(&local_sketches)
			.zip(&remote_sketches)
		{
			let residual = local_sketch
				.subtract(remote_sketch)
				.expect("subtract GF64 sketches");
			match residual.decode_elements(request.capacity) {
				| Ok(decoded) => roots.extend(decoded),
				| Err(_) => split(request, &mut next),
			}
		}
		frontier = next;
	}
	Adaptive64 { rounds, upload_bytes, roots }
}

struct Adaptive256 {
	rounds: usize,
	upload_bytes: usize,
	items: usize,
	roots: HashSet<[u8; 32]>,
}

fn adaptive256(
	local: &FixedWindowPopulation,
	remote: &FixedWindowPopulation,
	decoder: &Gf256Decoder,
) -> Adaptive256 {
	let mut frontier = vec![BucketRequest::new(0, 0, CAP)];
	let mut rounds = 0;
	let mut upload_bytes = 0;
	let mut roots = HashSet::new();
	for _ in 0..MAX_ROUNDS {
		if frontier.is_empty() {
			break;
		}
		rounds += 1;
		let local_sketches =
			build_gf256_bucket_sketches(local, &frontier).expect("build local GF256 sketches");
		let remote_sketches =
			build_gf256_bucket_sketches(remote, &frontier).expect("build remote GF256 sketches");
		upload_bytes += frontier
			.len()
			.saturating_mul(CAP)
			.saturating_mul(32);
		let mut next = Vec::new();
		for ((request, local_sketch), remote_sketch) in frontier
			.iter()
			.zip(&local_sketches)
			.zip(&remote_sketches)
		{
			let residual = local_sketch
				.subtract(remote_sketch)
				.expect("subtract GF256 sketches");
			match residual.decode_digests(request.capacity, decoder) {
				| Ok(decoded) => roots.extend(decoded),
				| Err(_) => split(request, &mut next),
			}
		}
		frontier = next;
	}
	Adaptive256 {
		rounds,
		upload_bytes,
		items: roots.len(),
		roots,
	}
}

fn split(request: &BucketRequest, next: &mut Vec<BucketRequest>) {
	if request.depth >= MAX_DEPTH {
		return;
	}
	let depth = request.depth.saturating_add(1);
	next.push(BucketRequest::new(depth, request.prefix.saturating_mul(2), CAP));
	next.push(BucketRequest::new(
		depth,
		request.prefix.saturating_mul(2).saturating_add(1),
		CAP,
	));
}

fn provisioned_bytes(estimate: u64) -> usize {
	let cap = CAP as u64;
	if estimate == 0 {
		return CAP.saturating_mul(32);
	}
	if estimate <= cap {
		let capacity = (estimate as f64 * 1.5).ceil() as u64 + 4;
		return (capacity.min(cap) as usize).saturating_mul(32);
	}
	let buckets = estimate.div_ceil(cap) as usize;
	buckets.saturating_mul(CAP).saturating_mul(32)
}

fn model_latency(
	rounds: f64,
	cpu_ms: f64,
	bytes: usize,
	rtt_ms: f64,
	bandwidth_mbps: f64,
) -> f64 {
	rounds * rtt_ms + cpu_ms + transfer_ms(bytes, bandwidth_mbps)
}

fn transfer_ms(bytes: usize, bandwidth_mbps: f64) -> f64 {
	(bytes as f64 * 8.0 * 1e3) / (bandwidth_mbps * 1e6)
}

fn format_bytes(bytes: usize) -> String {
	if bytes >= 1_048_576 {
		format!("{:.1}MiB", bytes as f64 / 1_048_576.0)
	} else if bytes >= 1_024 {
		format!("{:.1}KiB", bytes as f64 / 1_024.0)
	} else {
		format!("{bytes}B")
	}
}

fn avg_time(iters: u32, mut f: impl FnMut()) -> Duration {
	f();
	let start = Instant::now();
	for _ in 0..iters {
		f();
	}
	start.elapsed() / iters
}
