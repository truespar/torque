// Unit tests run on the server's allocator (`main.rs`), so the timings of the ignored benchmarks
// among them are the server's.
#[cfg(all(test, feature = "native", not(feature = "jemalloc"), target_env = "msvc"))]
#[global_allocator]
static TEST_ALLOC: mimalloc::MiMalloc = mimalloc::MiMalloc;

pub mod ai;
pub mod auth;
pub mod bitmap;
pub mod build;
pub mod collection;
pub mod config;
pub mod decay;
pub mod embedding;
pub mod filter;
pub mod gpu;
pub mod grouping;
pub mod index;
pub mod ingest;
pub mod ip_filter;
pub mod memory;
pub mod names;
pub mod observability;
pub mod persistence;
pub mod presets;
pub mod schema;
pub mod search;
pub mod server;
pub mod stemming;
pub mod stopwords;
pub mod analytics;
pub mod curation;
pub mod llm;
pub mod synonyms;
pub mod text;
pub mod tqbf;
pub mod vector;
