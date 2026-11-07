//! Text field lookups on a field with 1M terms over 3.78M documents: exact postings, the most
//! frequent completions of a prefix (a best-first walk of the term FST) and typo candidates (a
//! Levenshtein automaton over it).
use criterion::{criterion_group, criterion_main, Criterion};
use std::hint::black_box;
use torque::index::inverted::{typo_dfa, InvertedIndex, TermInput};

const DOCS: u32 = 3_780_000;
const TERMS: usize = 1_000_000;

fn next(s: &mut u64) -> u64 {
    *s ^= *s << 13;
    *s ^= *s >> 7;
    *s ^= *s << 17;
    *s
}

/// Words of 3-12 letters built from Swedish-like syllables, so that near neighbours exist.
fn vocabulary() -> Vec<String> {
    const SYL: [&str; 24] = [
        "st", "o", "ck", "holm", "ber", "g", "an", "ström", "lund", "vik", "a", "e", "fast", "ig", "het", "bygg",
        "kon", "sult", "han", "del", "ar", "en", "is", "ok",
    ];
    let mut s = 0x2545_f491_4f6c_dd1du64;
    let mut words: Vec<String> = Vec::with_capacity(TERMS + TERMS / 4);
    while words.len() < TERMS + TERMS / 4 {
        let parts = 1 + next(&mut s) % 4;
        let w: String = (0..parts).map(|_| SYL[(next(&mut s) % SYL.len() as u64) as usize]).collect();
        let w = crate_normalize(&w);
        if (3..=12).contains(&w.len()) {
            words.push(w);
        }
        // Rare tokens: syllables plus digits, like organisation numbers and street numbers.
        words.push(format!("{}{}", SYL[(next(&mut s) % 24) as usize], next(&mut s) % 1_000_000));
    }
    words.push("stockholm".into());
    words.sort_unstable();
    words.dedup();
    words.truncate(TERMS);
    words
}

/// The index stores accent-folded terms.
fn crate_normalize(w: &str) -> String {
    w.replace('ö', "o").replace('å', "a").replace('ä', "a")
}

fn field() -> InvertedIndex {
    let mut s = 7u64;
    let terms: Vec<TermInput> = vocabulary()
        .into_iter()
        .map(|term| {
            // Most terms are rare: half occur in one document, a few in thousands.
            let r = next(&mut s) % 100;
            let df = match r {
                0..=49 => 1,
                50..=89 => 2 + next(&mut s) % 20,
                90..=98 => 20 + next(&mut s) % 2000,
                _ => 2000 + next(&mut s) % 200_000,
            } as usize;
            let mut docs: Vec<u32> = (0..df).map(|_| (next(&mut s) % u64::from(DOCS)) as u32).collect();
            docs.sort_unstable();
            docs.dedup();
            let freqs = docs.iter().map(|d| 1 + d % 3).collect();
            TermInput { term: term.into_bytes(), docs, freqs }
        })
        .collect();
    InvertedIndex::build(&terms, &[], &vec![8; DOCS as usize]).unwrap()
}

fn bench_text(c: &mut Criterion) {
    let inv = field();
    println!("1M-term field: {} sections bytes", inv.section_bytes());
    let mut g = c.benchmark_group("text_1M_terms");
    g.bench_function("exact_postings", |b| b.iter(|| black_box(inv.postings(black_box(b"stockholm")))));
    g.bench_function("prefix_sto", |b| b.iter(|| black_box(inv.prefix(black_box(b"sto"), 4))));
    g.bench_function("prefix_s", |b| b.iter(|| black_box(inv.prefix(black_box(b"s"), 4))));
    g.bench_function("prefix_sto_256", |b| b.iter(|| black_box(inv.prefix(black_box(b"sto"), 256))));
    let (one, two) = (typo_dfa("stokholm", 1), typo_dfa("stokholm", 2));
    g.bench_function("typo1_stokholm", |b| b.iter(|| black_box(inv.fuzzy(&one))));
    g.bench_function("typo2_stokholm", |b| b.iter(|| black_box(inv.fuzzy(&two))));
    g.bench_function("dfa_typo2_stokholm", |b| b.iter(|| black_box(typo_dfa(black_box("stokholm"), 2))));
    g.finish();
}

criterion_group!(benches, bench_text);
criterion_main!(benches);
