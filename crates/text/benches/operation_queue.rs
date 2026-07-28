use clock::{Lamport, ReplicaId};
use criterion::{BatchSize, Criterion, black_box, criterion_group, criterion_main};
use rand::{SeedableRng, rngs::StdRng, seq::SliceRandom};
use text::operation_queue::{Operation, OperationQueue};

#[derive(Clone, Debug)]
struct TestOperation(Lamport);

impl Operation for TestOperation {
    fn lamport_timestamp(&self) -> Lamport {
        self.0
    }
}

fn operation_queue_insertion(c: &mut Criterion) {
    let mut clock = Lamport::new(ReplicaId::LOCAL);
    let mut operations = (0..10_000)
        .map(|_| TestOperation(clock.tick()))
        .collect::<Vec<_>>();
    operations.shuffle(&mut StdRng::seed_from_u64(1));

    c.bench_function("insert_10k_unique_shuffled_operations", |b| {
        b.iter_batched(
            || operations.clone(),
            |operations| {
                let mut queue = OperationQueue::new();
                queue.insert(operations);
                black_box(queue.len());
            },
            BatchSize::SmallInput,
        );
    });
}

criterion_group!(benches, operation_queue_insertion);
criterion_main!(benches);
