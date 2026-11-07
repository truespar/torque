//! GEO section lookups over 3.78M documents with a point each (95% present), placed like company
//! addresses: clusters around Stockholm, Gothenburg and Malmö, the rest spread over Sweden, at the
//! 5 decimals real coordinates have. Radius filters, nearest-first pages and per-document
//! distances all run on the section in place.
use criterion::{criterion_group, criterion_main, Criterion};
use std::hint::black_box;
use std::time::Instant;
use torque::bitmap::Bitmap;
use torque::index::geo::{e6, sort_centre, Cap, GeoIndex};

const DOCS: u32 = 3_780_000;

fn next(s: &mut u64) -> u64 {
    *s ^= *s << 13;
    *s ^= *s >> 7;
    *s ^= *s << 17;
    *s
}

fn unit(s: &mut u64) -> f64 {
    (next(s) >> 11) as f64 / (1u64 << 53) as f64
}

/// Roughly normal (sum of uniforms), mean 0, sd 1.
fn normal(s: &mut u64) -> f64 {
    (0..6).map(|_| unit(s)).sum::<f64>() - 3.0
}

fn index() -> GeoIndex {
    let mut s = 0x2545_f491_4f6c_dd1du64;
    let mut pts = Vec::with_capacity(DOCS as usize);
    for d in 0..DOCS {
        if next(&mut s) % 20 == 0 {
            continue;
        }
        let r = unit(&mut s);
        let (lat, lng) = if r < 0.35 {
            (59.33 + 0.25 * normal(&mut s), 18.07 + 0.45 * normal(&mut s))
        } else if r < 0.50 {
            (57.71 + 0.2 * normal(&mut s), 11.97 + 0.35 * normal(&mut s))
        } else if r < 0.60 {
            (55.60 + 0.15 * normal(&mut s), 13.00 + 0.25 * normal(&mut s))
        } else {
            (55.3 + 13.5 * unit(&mut s), 11.1 + 12.5 * unit(&mut s))
        };
        let round5 = |x: f64| (x * 1e5).round() / 1e5;
        pts.push((d, e6(round5(lat)), e6(round5(lng))));
    }
    let t = Instant::now();
    let idx = GeoIndex::build(&pts, DOCS, false).unwrap();
    println!(
        "built {} points in {:.2?}: {} bytes ({:.2} bytes/point, {:.2} bytes/doc)",
        pts.len(),
        t.elapsed(),
        idx.section_bytes(),
        idx.section_bytes() as f64 / pts.len() as f64,
        idx.section_bytes() as f64 / f64::from(DOCS)
    );
    idx
}

fn bench(c: &mut Criterion) {
    let idx = index();
    let stockholm = (59.3293, 18.0686);
    let all: Bitmap = (0..DOCS).collect();
    let ten_pct: Bitmap = (0..DOCS).step_by(10).collect();
    for r in [1_000.0, 10_000.0, 50_000.0] {
        let cap = Cap::new(stockholm.0, stockholm.1, r);
        let n = idx.radius(&cap).cardinality();
        c.bench_function(&format!("radius {} km around Stockholm ({n} docs)", r / 1000.0), |b| {
            b.iter(|| black_box(idx.radius(&cap).cardinality()))
        });
    }
    let centre = sort_centre(stockholm.0, stockholm.1);
    c.bench_function("nearest 10 from Stockholm, all docs", |b| b.iter(|| black_box(idx.nearest(centre, &all, 10))));
    c.bench_function("nearest 10 from Stockholm, 10% of docs", |b| b.iter(|| black_box(idx.nearest(centre, &ten_pct, 10))));
    c.bench_function("nearest 250 from Kiruna, all docs", |b| {
        b.iter(|| black_box(idx.nearest(sort_centre(67.8558, 20.2253), &all, 250)))
    });
    let mut d = 0u32;
    c.bench_function("distance of one document", |b| {
        b.iter(|| {
            d = (d + 7919) % DOCS;
            black_box(idx.distance_m(d, centre))
        })
    });
}

criterion_group! {
    name = benches;
    config = Criterion::default().sample_size(20);
    targets = bench
}
criterion_main!(benches);
