use criterion::{BatchSize, Criterion, black_box, criterion_group, criterion_main};
use multi_buffer::PathKey;
use rand::{SeedableRng, rngs::StdRng, seq::SliceRandom};

fn edited_path_sorting(c: &mut Criterion) {
    let mut paths = (0..10_000)
        .map(|index| (PathKey::sorted(index), index))
        .collect::<Vec<_>>();
    paths.shuffle(&mut StdRng::seed_from_u64(1));

    let mut group = c.benchmark_group("sort_10k_edited_paths");
    group.bench_function("cloned_key", |b| {
        b.iter_batched(
            || paths.clone(),
            |mut paths| {
                paths.sort_unstable_by_key(|(path, _)| path.clone());
                black_box(paths);
            },
            BatchSize::SmallInput,
        );
    });
    group.bench_function("borrowed_comparator", |b| {
        b.iter_batched(
            || paths.clone(),
            |mut paths| {
                paths.sort_unstable_by(|a, b| a.0.cmp(&b.0));
                black_box(paths);
            },
            BatchSize::SmallInput,
        );
    });
    group.finish();
}

criterion_group!(benches, edited_path_sorting);
criterion_main!(benches);
