#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::hint::black_box;
use std::sync::Arc;

use cofre_core::chunker::ChunkerParams;
use cofre_core::crypto::KdfParams;
use cofre_core::{Backend, InitOptions, LocalBackend, Repository};
use criterion::{Criterion, Throughput, criterion_group, criterion_main};

fn data(len: usize) -> Vec<u8> {
    let mut x: u64 = 0x9e37_79b9_7f4a_7c15;
    let mut out = Vec::with_capacity(len);
    while out.len() < len {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        out.extend_from_slice(&x.to_le_bytes());
    }
    out.truncate(len);
    out
}

fn bench(c: &mut Criterion) {
    let dir = tempfile::TempDir::new().unwrap();
    let backend: Arc<dyn Backend> = Arc::new(LocalBackend::new(dir.path()));
    let repo = Repository::init(
        backend,
        b"bench",
        InitOptions {
            kdf: KdfParams::new(1024, 1, 1),
            ..Default::default()
        },
    )
    .unwrap();

    let input = data(16 * 1024 * 1024);
    let mut group = c.benchmark_group("pipeline");
    group.throughput(Throughput::Bytes(input.len() as u64));
    group.sample_size(10);

    group.bench_function("fastcdc", |b| {
        b.iter(|| {
            let n = ChunkerParams::default()
                .chunks(black_box(input.as_slice()))
                .count();
            black_box(n)
        })
    });

    group.bench_function("blake3_keyed", |b| {
        b.iter(|| black_box(repo.chunk_id(black_box(&input))))
    });

    let chunk = &input[..1024 * 1024];
    let id = repo.chunk_id(chunk);
    group.throughput(Throughput::Bytes(chunk.len() as u64));
    group.bench_function("zstd_xchacha_encode_1mib", |b| {
        b.iter(|| black_box(repo.encode_chunk(&id, black_box(chunk)).unwrap()))
    });
    let blob = repo.encode_chunk(&id, chunk).unwrap();
    group.bench_function("decode_verify_1mib", |b| {
        b.iter(|| black_box(repo.decode_chunk(&id, black_box(&blob)).unwrap()))
    });
    group.finish();

    c.bench_function("argon2id_64mib_t3", |b| {
        let params = KdfParams::recommended();
        b.iter(|| black_box(params.derive(b"senha").unwrap()))
    });
}

criterion_group!(benches, bench);
criterion_main!(benches);
