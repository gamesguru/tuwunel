//! ```cargo
//! [dependencies]
//! roaring = "0.11.4"
//! ```

use std::{collections::BTreeMap, time::Instant};

use roaring::RoaringBitmap;

// --- 1. Origin/Main BTreeMap counts approach ---
struct Counts<Id: Ord> {
	by_id: BTreeMap<Id, usize>,
	total: usize,
}

impl<Id: Ord> Default for Counts<Id> {
	fn default() -> Self { Self { by_id: BTreeMap::new(), total: 0 } }
}

impl<Id: Ord + Clone> Counts<Id> {
	fn merge(mut self, set: &[Id]) -> Self {
		self.total = self.total.saturating_add(1);
		for id in set {
			let count = self.by_id.entry(id.clone()).or_default();
			*count = count.saturating_add(1);
		}
		self
	}

	fn result(self) -> Vec<Id> {
		let total = self.total;
		self.by_id
			.into_iter()
			.filter_map(move |(id, count)| (count < total).then_some(id))
			.collect()
	}
}

fn run_benchmark_for_config(num_sets: usize, size: usize, runs: u32) {
	println!("=== Benchmark: {size} elements across {num_sets} auth sets ({runs} runs) ===");

	// Generate overlapping auth sets simulating Matrix event IDs
	let mut sets: Vec<Vec<String>> = vec![Vec::new(); num_sets];
	for i in 0..size {
		let id = format!("$event_id_string_representation_for_testing_{i:05}");
		for (s, set) in sets.iter_mut().enumerate() {
			if i % (s + 3) != 0 {
				set.push(id.clone());
			}
		}
	}

	// Pre-build index mapping and RoaringBitmaps for the cached scenario
	let mut id_to_index = BTreeMap::new();
	let mut index_to_id = Vec::new();
	let mut cached_bitmaps = Vec::new();

	for set in &sets {
		let mut bitmap = RoaringBitmap::new();
		for id in set {
			let idx = match id_to_index.get(id) {
				| Some(&idx) => idx,
				| None => {
					let idx = u32::try_from(index_to_id.len()).unwrap();
					id_to_index.insert(id.clone(), idx);
					index_to_id.push(id.clone());
					idx
				},
			};
			bitmap.insert(idx);
		}
		cached_bitmaps.push(bitmap);
	}

	// 1. Measure Origin/Main BTreeMap counts approach (re-allocates/counts strings)
	let start = Instant::now();
	for _ in 0..runs {
		let mut counts = Counts::default();
		for set in &sets {
			counts = counts.merge(set);
		}
		let _res = counts.result();
	}
	let origin_duration = start.elapsed();
	println!(
		"Origin/Main (BTreeMap Counts):        {:?} (avg: {:?})",
		origin_duration,
		origin_duration / runs
	);

	// 2. Measure PRE-COMPUTED/CACHED Roaring (Bitwise union/intersection diff)
	let start = Instant::now();
	for _ in 0..runs {
		let diff = if num_sets == 2 {
			// Fast path for 2-set resolution: symmetric difference (XOR)
			&cached_bitmaps[0] ^ &cached_bitmaps[1]
		} else {
			let mut union = RoaringBitmap::new();
			let mut intersection = RoaringBitmap::new();
			let mut first = true;

			for bitmap in &cached_bitmaps {
				if first {
					union.clone_from(bitmap);
					intersection = bitmap.clone();
					first = false;
				} else {
					union |= bitmap;
					intersection &= bitmap;
				}
			}
			union - intersection
		};

		let _result_ids: Vec<String> = diff
			.into_iter()
			.map(|idx| {
				let index = usize::try_from(idx).unwrap();
				index_to_id[index].clone()
			})
			.collect();
	}
	let roaring_duration = start.elapsed();
	println!(
		"Cached/Pre-computed Roaring:         {:?} (avg: {:?})",
		roaring_duration,
		roaring_duration / runs
	);

	let speedup = origin_duration.as_nanos() as f64 / roaring_duration.as_nanos() as f64;
	println!("Speedup of Cached Roaring over Origin/Main: {speedup:.2}x\n");
}

fn main() {
	println!("Starting Comparative Performance Benchmark...\n");

	// 1. 2-Set Conflict Resolution (Most common state fork)
	println!("--------------------------------------------------");
	println!(">>> 2-SET SCENARIOS (Direct 2-Branch Conflict) <<<");
	println!("--------------------------------------------------");
	run_benchmark_for_config(2, 100, 50_000);
	run_benchmark_for_config(2, 500, 20_000);
	run_benchmark_for_config(2, 2000, 10_000);

	// 2. Multi-Set Conflict Resolution (20 Overlapping Branches)
	println!("--------------------------------------------------");
	println!(">>> 20-SET SCENARIOS (Complex Multi-Branch DAG) <<<");
	println!("--------------------------------------------------");
	run_benchmark_for_config(20, 100, 20_000);
	run_benchmark_for_config(20, 2000, 5_000);
}
