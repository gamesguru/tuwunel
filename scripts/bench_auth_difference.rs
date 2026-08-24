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
	fn merge(mut self, set: Vec<Id>) -> Self {
		self.total = self.total.saturating_add(1);
		for id in set {
			let count = self.by_id.entry(id).or_default();
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

fn main() {
	println!("Starting Comparative Performance Benchmark...\n");

	let runs = 20_000;
	let num_sets = 20; // 20 overlapping auth sets

	for &size in &[100, 2000] {
		println!("--- Benchmark with size: {size} elements across {num_sets} sets ---");

		// We use String to simulate Event ID representation exactly
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

		// 1. Measure Origin/Main BTreeMap counts approach (Must re-parse, allocate and merge strings)
		let start = Instant::now();
		for _ in 0..runs {
			let mut counts = Counts::default();
			for set in &sets {
				counts = counts.merge(set.clone());
			}
			let _res = counts.result();
		}
		let origin_duration = start.elapsed();
		println!(
			"Origin/Main (BTreeMap Counts):     {:?} (avg: {:?})",
			origin_duration,
			origin_duration / runs
		);

		// 2. Measure PRE-COMPUTED/CACHED Roaring Sub::sub Approach (Just bitwise operations)
		let start = Instant::now();
		for _ in 0..runs {
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

			let diff = std::ops::Sub::sub(union, intersection);
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
			"Cached/Pre-computed Roaring:      {:?} (avg: {:?})",
			roaring_duration,
			roaring_duration / runs
		);

		let speedup = origin_duration.as_nanos() as f64 / roaring_duration.as_nanos() as f64;
		println!("Speedup of Cached Roaring over Origin/Main: {speedup:.2}x\n");
	}
}
