//! Value index lookups over 3.78M documents, shaped like the company collection: a 21-value county
//! facet field, a 15K-value registration date (days since 1970) sort field, a high-cardinality
//! amount present in 1 of 5 documents, and a string[] facet field. Exact and range filters, facet
//! counts, sorting and per-document reads all run on the sections in place.
use criterion::{criterion_group, criterion_main, Criterion};
use std::hint::black_box;
use std::ops::Bound;
use torque::bitmap::Bitmap;
use torque::index::facet::ordinal_counts;
use torque::index::sort::{Missing, SortColumn};
use torque::index::values::{Ords, ValueCollector, ValueIndex};
use torque::index::Segment;

const DOCS: u32 = 3_780_000;

fn next(s: &mut u64) -> u64 {
    *s ^= *s << 13;
    *s ^= *s >> 7;
    *s ^= *s << 17;
    *s
}

struct Fields {
    seg: Segment,
    county: u16,
    date: u16,
    amount: u16,
    sni: u16,
}

fn fields() -> Fields {
    let mut s = 0x9e37_79b9_7f4a_7c15u64;
    let mut county = ValueCollector::<i64>::new(DOCS, Ords::Single);
    let mut date = ValueCollector::<i64>::new(DOCS, Ords::Single);
    let mut amount = ValueCollector::<i64>::new(DOCS, Ords::Single);
    let mut sni = ValueCollector::<String>::new(DOCS, Ords::Multi);
    for d in 0..DOCS {
        // Skewed: a third of the documents in the first county, as Stockholm.
        let r = next(&mut s) % 1000;
        let c = if r < 330 { 1 } else { 2 + (r % 20) as i64 };
        county.add(d, c);
        if r % 50 != 0 {
            date.add(d, (next(&mut s) % 15_000) as i64);
        }
        if r % 5 == 0 {
            amount.add(d, (next(&mut s) % 50_000_000) as i64);
        }
        if r % 2 == 0 {
            let n = 1 + next(&mut s) % 3;
            sni.add_many(d, (0..n).map(|_| Some(format!("{:05}", next(&mut s) % 800 * 97 % 99_999))));
        }
    }
    let mut seg = Segment::empty();
    seg.values.insert(0, ValueIndex::build_numbers(county, false).unwrap());
    seg.values.insert(1, ValueIndex::build_numbers(date, false).unwrap());
    seg.values.insert(2, ValueIndex::build_numbers(amount, false).unwrap());
    seg.values.insert(3, ValueIndex::build_strings(sni).unwrap());
    seg.all_docs.add_range(0..DOCS);
    for (name, f) in [("county", 0u16), ("date", 1), ("amount", 2), ("sni", 3)] {
        let v = &seg.values[&f];
        println!("{name}: {} values, {} bytes ({:.2} bytes/doc)", v.len(), v.section_bytes(), v.section_bytes() as f64 / f64::from(DOCS));
    }
    Fields { seg, county: 0, date: 1, amount: 2, sni: 3 }
}

fn bench(c: &mut Criterion) {
    let f = fields();
    let county = &f.seg.values[&f.county];
    let date = &f.seg.values[&f.date];
    let amount = &f.seg.values[&f.amount];
    let sni = &f.seg.values[&f.sni];
    let one_pct: Bitmap = (0..DOCS).step_by(100).collect();
    let ten_pct: Bitmap = (0..DOCS).step_by(10).collect();

    c.bench_function("eq county=14 (~180K docs)", |b| b.iter(|| black_box(county.eq_key(14).unwrap().cardinality())));
    c.bench_function("eq sni string (~4.7K docs)", |b| b.iter(|| black_box(sni.eq_str("00097").map(|bm| bm.cardinality()))));
    c.bench_function("range date 10 years (~880K docs)", |b| {
        b.iter(|| black_box(date.union(date.key_ords(Bound::Included(10_000), Bound::Excluded(13_650))).cardinality()))
    });
    c.bench_function("range amount > 40M (~150K docs, mostly single-doc values)", |b| {
        b.iter(|| black_box(amount.union(amount.key_ords(Bound::Excluded(40_000_000), Bound::Unbounded)).cardinality()))
    });
    c.bench_function("facet sni over all docs", |b| b.iter(|| black_box(ordinal_counts(&f.seg.all_docs, sni, None).len())));
    c.bench_function("facet sni over 1% (ordinals)", |b| b.iter(|| black_box(ordinal_counts(&one_pct, sni, None).len())));
    let col = SortColumn::of(&f.seg, f.date).unwrap();
    c.bench_function("sort date desc top 10 over all docs (value walk)", |b| b.iter(|| black_box(col.presorted(&f.seg.all_docs, true, col.missing_first(true, Missing::Normal), 10))));
    c.bench_function("sort date asc top 10 over 10% (keys + select)", |b| {
        b.iter(|| {
            let mut pairs: Vec<(u64, u32)> = ten_pct.iter().map(|d| (col.key(d, false, col.missing_first(false, Missing::Normal)), d)).collect();
            pairs.select_nth_unstable(9);
            black_box(pairs[0])
        })
    });
    c.bench_function("doc value date (1M reads)", |b| {
        b.iter(|| {
            let mut sum = 0i64;
            for d in (0..DOCS).step_by(3).take(1_000_000) {
                sum += date.doc_i64(d).unwrap_or(0);
            }
            black_box(sum)
        })
    });
}

criterion_group! {
    name = benches;
    config = Criterion::default().sample_size(20);
    targets = bench
}
criterion_main!(benches);
