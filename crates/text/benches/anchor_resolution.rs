use clock::ReplicaId;
use criterion::{Criterion, black_box, criterion_group, criterion_main};
use text::{Buffer, BufferId, ToOffset};

fn anchor_resolution(c: &mut Criterion) {
    let text = "abcdefghij\n".repeat(100_000);
    let mut buffer = Buffer::new(
        ReplicaId::LOCAL,
        BufferId::new(1).expect("buffer id should be nonzero"),
        text,
    );
    for offset in (0..buffer.len()).step_by(1_000).rev() {
        buffer.edit([(offset..offset, "x")]);
    }

    let snapshot = buffer.snapshot();
    let anchors = (0..snapshot.len())
        .step_by(100)
        .map(|offset| snapshot.anchor_before(offset))
        .collect::<Vec<_>>();

    c.bench_function("anchor_to_offset_fragmented_buffer", |b| {
        b.iter(|| {
            for anchor in &anchors {
                black_box(anchor.to_offset(black_box(&snapshot)));
            }
        });
    });
}

criterion_group!(benches, anchor_resolution);
criterion_main!(benches);
