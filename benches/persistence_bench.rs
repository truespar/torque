// The server's allocator (src/main.rs): what a bench measures is what the server runs.
#[cfg(all(feature = "native", not(feature = "jemalloc"), target_env = "msvc"))]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

use criterion::{criterion_group, criterion_main, Criterion};
use std::time::Instant;

use torque::ingest::build_index;
use torque::schema::{Document, FieldDef, FieldType, FieldValue, Language, Schema};

fn make_schema(name: &str) -> Schema {
    let field = |name: &str, field_type: FieldType, facet: bool, index: bool, sort: bool| FieldDef { name: name.into(), field_type, facet, index, sort, ..Default::default() };
    let mut s = Schema {
        name: name.into(),
        fields: vec![
            field("title", FieldType::String, false, true, false),
            field("price", FieldType::Int32, false, false, true),
            field("category", FieldType::String, true, true, true),
        ],
        default_language: Language::English,
        ..Default::default()
    };
    s.assign_field_ids();
    s
}

fn generate_docs(n: usize) -> Vec<Document> {
    let categories = ["Electronics", "Books", "Clothing", "Home", "Sports"];
    let words = ["quick", "brown", "fox", "jumps", "over", "lazy", "dog", "runs", "fast", "slow"];
    (0..n).map(|i| {
        let title = format!("{} {} {} product-{}", words[i % 10], words[(i * 3) % 10], words[(i * 7) % 10], i);
        Document {
            id: i as u32,
            external_id: format!("doc-{i}"),
            language: Language::English,
            fields: vec![
                FieldValue::Text(title),
                FieldValue::Int32((i % 10000) as i32),
                FieldValue::Text(categories[i % 5].into()),
            ].into(),
            record: None,
        }
    }).collect()
}

/// Single-run timing for each scale. Prints a results table at the end.
fn bench_persistence(_c: &mut Criterion) {
    let mut results: Vec<(String, f64, f64, f64, String)> = Vec::new();

    for &(label, n) in &[("100K", 100_000), ("200K", 200_000), ("300K", 300_000), ("1M", 1_000_000)] {
        let schema = make_schema("bench_col");

        eprintln!("\n=== {} ({} docs) ===", label, n);

        eprintln!("  Generating docs...");
        let t0 = Instant::now();
        let docs = generate_docs(n);
        eprintln!("  Doc generation: {:.2}s", t0.elapsed().as_secs_f64());

        eprintln!("  Building index...");
        let t1 = Instant::now();
        let index = build_index(&schema, docs);
        let build_time = t1.elapsed().as_secs_f64();
        eprintln!("  Index build: {:.2}s", build_time);

        let tmp_dir = tempfile::tempdir().unwrap();
        let data_dir = tmp_dir.path();

        eprintln!("  Saving index...");
        let t2 = Instant::now();
        let col_dir = data_dir.join("indexes").join("bench_col");
        std::fs::create_dir_all(&col_dir).unwrap();
        let segment_path = col_dir.join("segment.tqbf");
        torque::persistence::index::write_segment(&index, None, &segment_path).unwrap();
        let save_time = t2.elapsed().as_secs_f64();
        eprintln!("  Save: {:.2}s", save_time);

        // Report file sizes
        let mut disk_info = String::new();
        if col_dir.exists() {
            let mut total = 0u64;
            let mut parts = Vec::new();
            for entry in std::fs::read_dir(&col_dir).unwrap() {
                let entry = entry.unwrap();
                let size = entry.metadata().unwrap().len();
                total += size;
                let name = entry.file_name().to_string_lossy().to_string();
                if size > 1024 * 1024 {
                    parts.push(format!("{}={:.1}MB", name, size as f64 / (1024.0 * 1024.0)));
                } else {
                    parts.push(format!("{}={:.0}KB", name, size as f64 / 1024.0));
                }
            }
            parts.sort();
            disk_info = format!("{:.1} MB total", total as f64 / (1024.0 * 1024.0));
            eprintln!("  Disk: {} ({})", disk_info, parts.join(", "));
        }

        // Drop in-memory index before loading to reduce peak memory
        drop(index);

        eprintln!("  Loading index...");
        let t3 = Instant::now();
        let idx = torque::persistence::index::open_segment(&segment_path, &schema, 1).unwrap();
        let load_time = t3.elapsed().as_secs_f64();
        assert_eq!(idx.total_docs() as usize, n);
        eprintln!("  Load: {:.2}s", load_time);

        results.push((label.to_string(), build_time, save_time, load_time, disk_info));
    }

    // Print summary table
    eprintln!("\n╔══════════╦════════════╦════════════╦════════════╗");
    eprintln!("║  Scale   ║   Build    ║    Save    ║    Load    ║");
    eprintln!("╠══════════╬════════════╬════════════╬════════════╣");
    for (label, build, save, load, _) in &results {
        eprintln!("║ {:>8} ║ {:>8.2}s  ║ {:>8.2}s  ║ {:>8.2}s  ║", label, build, save, load);
    }
    eprintln!("╚══════════╩════════════╩════════════╩════════════╝");

    eprintln!("\nDisk sizes:");
    for (label, _, _, _, disk) in &results {
        eprintln!("  {}: {}", label, disk);
    }
}

criterion_group!(benches, bench_persistence);
criterion_main!(benches);
