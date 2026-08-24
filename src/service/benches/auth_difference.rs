#![cfg(test)]

use std::{collections::BTreeMap, hint::black_box};

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use roaring::RoaringBitmap;

// 1. Legacy / Naive BTreeMap counts approach
#[derive(Default)]
struct BTreeCounts<Id: Ord> {
	by_id: BTreeMap<Id, usize>,
	total: usize,
}

impl<Id: Ord + Clone> BTreeCounts<Id> {
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

// 2. Dynamic RoaringBitmap state approach (with ID mapping)
#[derive(Default)]
struct DynamicRoaringState<Id: Ord> {
	id_to_index: BTreeMap<Id, u32>,
	index_to_id: Vec<Id>,
	union: RoaringBitmap,
	intersection: RoaringBitmap,
	first: bool,
}

impl<Id: Ord + Clone> DynamicRoaringState<Id> {
	fn new() -> Self {
		Self {
			id_to_index: BTreeMap::new(),
			index_to_id: Vec::new(),
			union: RoaringBitmap::new(),
			intersection: RoaringBitmap::new(),
			first: true,
		}
	}

	fn merge(mut self, set: &[Id]) -> Self {
		let mut bitmap = RoaringBitmap::new();
		for id in set {
			let idx = match self.id_to_index.get(id) {
				| Some(&idx) => idx,
				| None => {
					let idx = u32::try_from(self.index_to_id.len()).expect("too many event IDs");
					self.id_to_index.insert(id.clone(), idx);
					self.index_to_id.push(id.clone());
					idx
				},
			};
			bitmap.insert(idx);
		}

		if self.first {
			self.union.clone_from(&bitmap);
			self.intersection = bitmap;
			self.first = false;
		} else {
			self.union |= &bitmap;
			self.intersection &= bitmap;
		}

		self
	}

	fn result(self) -> Vec<Id> {
		if self.first {
			Vec::new()
		} else {
			let diff = self.union - self.intersection;
			diff.into_iter()
				.map(move |idx| {
					let index = usize::try_from(idx).expect("idx fits in usize");
					self.index_to_id[index].clone()
				})
				.collect()
		}
	}
}

fn generate_auth_sets(size: usize, num_sets: usize) -> Vec<Vec<String>> {
	let mut sets = vec![Vec::new(); num_sets];
	for i in 0..size {
		let id = format!("$event_id_string_representation_for_testing_{:05}", i);
		for (s, set) in sets.iter_mut().enumerate() {
			if i % (s.saturating_add(3)) != 0 {
				set.push(id.clone());
			}
		}
	}
	sets
}

fn bench_auth_difference(c: &mut Criterion) {
	let mut group = c.benchmark_group("auth_difference");
	let num_sets = 20;

	for &size in &[100, 500, 2000] {
		let sets = generate_auth_sets(size, num_sets);
		group.throughput(Throughput::Elements(size as u64));

		// 1. BTreeMap Counts
		group.bench_with_input(
			BenchmarkId::new("btree_counts", size),
			&sets,
			|b, sets| {
				b.iter(|| {
					let mut counts = BTreeCounts::default();
					for set in sets {
						counts = counts.merge(set);
					}
					black_box(counts.result())
				});
			},
		);

		// 2. Dynamic RoaringBitmap (live index mapping)
		group.bench_with_input(
			BenchmarkId::new("dynamic_roaring", size),
			&sets,
			|b, sets| {
				b.iter(|| {
					let mut state = DynamicRoaringState::new();
					for set in sets {
						state = state.merge(set);
					}
					black_box(state.result())
				});
			},
		);

		// 3. Pre-cached RoaringBitmaps (pure bitwise operations + ID projection)
		let mut id_to_index = BTreeMap::new();
		let mut index_to_id = Vec::new();
		let mut cached_bitmaps = Vec::new();

		for set in &sets {
			let mut bitmap = RoaringBitmap::new();
			for id in set {
				let idx = match id_to_index.get(id) {
					| Some(&idx) => idx,
					| None => {
						let idx = u32::try_from(index_to_id.len()).expect("too many event IDs");
						id_to_index.insert(id.clone(), idx);
						index_to_id.push(id.clone());
						idx
					},
				};
				bitmap.insert(idx);
			}
			cached_bitmaps.push(bitmap);
		}

		group.bench_with_input(
			BenchmarkId::new("cached_roaring_bitwise", size),
			&cached_bitmaps,
			|b, bitmaps| {
				b.iter(|| {
					let mut union = RoaringBitmap::new();
					let mut intersection = RoaringBitmap::new();
					let mut first = true;

					for bitmap in bitmaps {
						if first {
							union.clone_from(bitmap);
							intersection = bitmap.clone();
							first = false;
						} else {
							union |= bitmap;
							intersection &= bitmap;
						}
					}

					let diff = union - intersection;
					let result: Vec<String> = diff
						.into_iter()
						.map(|idx| {
							let index = usize::try_from(idx).expect("idx fits in usize");
							index_to_id[index].clone()
						})
						.collect();
					black_box(result)
				});
			},
		);
	}

	group.finish();
}

criterion_group!(benches, bench_auth_difference);
criterion_main!(benches);
