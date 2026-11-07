//! crate::bitmap (owned and frozen) against roaring-rs on posting-list shapes of a 3.78M-doc segment.
use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use roaring::RoaringBitmap;
use std::hint::black_box;
use torque::bitmap::Bitmap;

const UNIVERSE: u32 = 3_780_000;

fn values(n: usize, seed: u64) -> Vec<u32> {
    let mut s = seed;
    let mut v: Vec<u32> = (0..n)
        .map(|_| {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            (s % u64::from(UNIVERSE)) as u32
        })
        .collect();
    v.sort_unstable();
    v.dedup();
    v
}

struct Shape {
    name: &'static str,
    a: Vec<u32>,
    b: Vec<u32>,
}

fn shapes() -> Vec<Shape> {
    let range: Vec<u32> = (100_000..3_000_000).collect();
    vec![
        Shape { name: "sparse&dense", a: values(10_000, 1), b: values(1_000_000, 2) },
        Shape { name: "dense&dense", a: values(1_000_000, 3), b: values(800_000, 4) },
        Shape { name: "dense&range", a: values(1_000_000, 5), b: range },
        Shape { name: "sparse&sparse", a: values(20_000, 6), b: values(30_000, 7) },
    ]
}

fn ours(v: &[u32], frozen: bool) -> Bitmap {
    let mut b: Bitmap = v.iter().copied().collect();
    b.run_optimize();
    if frozen {
        b = Bitmap::deserialize_portable(&b.serialize_portable()).unwrap();
    }
    b
}

fn bench_binary(c: &mut Criterion) {
    for s in shapes() {
        let (ra, rb): (RoaringBitmap, RoaringBitmap) = (s.a.iter().copied().collect(), s.b.iter().copied().collect());
        let (oa, ob) = (ours(&s.a, false), ours(&s.b, false));
        let (fa, fb) = (ours(&s.a, true), ours(&s.b, true));
        let mut g = c.benchmark_group(format!("bitmap/{}", s.name));
        g.bench_function(BenchmarkId::new("and", "roaring-rs"), |x| x.iter(|| black_box(&ra & &rb)));
        g.bench_function(BenchmarkId::new("and", "torque"), |x| x.iter(|| black_box(&oa & &ob)));
        g.bench_function(BenchmarkId::new("and", "torque-frozen"), |x| x.iter(|| black_box(&fa & &fb)));
        g.bench_function(BenchmarkId::new("or", "roaring-rs"), |x| x.iter(|| black_box(&ra | &rb)));
        g.bench_function(BenchmarkId::new("or", "torque"), |x| x.iter(|| black_box(&oa | &ob)));
        g.bench_function(BenchmarkId::new("or", "torque-frozen"), |x| x.iter(|| black_box(&fa | &fb)));
        g.bench_function(BenchmarkId::new("andnot", "roaring-rs"), |x| x.iter(|| black_box(&ra - &rb)));
        g.bench_function(BenchmarkId::new("andnot", "torque"), |x| x.iter(|| black_box(&oa - &ob)));
        g.bench_function(BenchmarkId::new("and_card", "roaring-rs"), |x| x.iter(|| black_box(ra.intersection_len(&rb))));
        g.bench_function(BenchmarkId::new("and_card", "torque-frozen"), |x| x.iter(|| black_box(fa.and_cardinality(&fb))));
        g.bench_function(BenchmarkId::new("iter_sum", "roaring-rs"), |x| x.iter(|| black_box(ra.iter().map(u64::from).sum::<u64>())));
        g.bench_function(BenchmarkId::new("iter_sum", "torque-frozen"), |x| x.iter(|| black_box(fa.iter().map(u64::from).sum::<u64>())));
        let probes = values(1000, 99);
        g.bench_function(BenchmarkId::new("contains_1000", "roaring-rs"), |x| x.iter(|| black_box(probes.iter().filter(|&&p| rb.contains(p)).count())));
        g.bench_function(BenchmarkId::new("contains_1000", "torque-frozen"), |x| x.iter(|| black_box(probes.iter().filter(|&&p| fb.contains(p)).count())));
        g.finish();
    }
}

fn bench_union_many(c: &mut Criterion) {
    // Numeric range filter today: one small bitmap per distinct value, unioned.
    let parts: Vec<Vec<u32>> = (0..2000).map(|i| values(50, 1000 + i)).collect();
    let r: Vec<RoaringBitmap> = parts.iter().map(|v| v.iter().copied().collect()).collect();
    let o: Vec<Bitmap> = parts.iter().map(|v| ours(v, true)).collect();
    let mut g = c.benchmark_group("bitmap/union_many_2000x50");
    g.bench_function("roaring-rs", |x| {
        use roaring::MultiOps;
        x.iter(|| black_box(r.iter().union()))
    });
    g.bench_function("torque-frozen", |x| x.iter(|| black_box(Bitmap::union_many(o.iter()))));
    g.finish();
}

criterion_group!(benches, bench_binary, bench_union_many);
criterion_main!(benches);
