use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::time::Instant;

use clap::{Parser, Subcommand};
use indicatif::{ProgressBar, ProgressStyle};
use torque::ingest::protocol::{
    encode_delete_ids, serialize_client_message, serialize_document,
    deserialize_server_message, ClientMessage, ServerMessage,
};
use torque::ingest::parse_json_document;
use torque::schema::Schema;

#[derive(Parser)]
#[command(name = "torque-admin", about = "Torque admin CLI — build, upload, export and import collections")]
struct Cli {
    /// Torque HTTP base URL (127.0.0.1, not localhost: Windows tries ::1 first and waits 2 s)
    #[arg(long, default_value = "http://127.0.0.1:8108", global = true)]
    url: String,

    /// API key for Torque HTTP API (set via flag or TORQUE_API_KEY env var; every command except
    /// `build` needs it)
    #[arg(long, env = "TORQUE_API_KEY", global = true)]
    api_key: Option<String>,

    /// TCP ingest address (host:port)
    #[arg(long, default_value = "127.0.0.1:8119", global = true)]
    tcp_addr: String,

    #[command(subcommand)]
    command: Command,
}

#[derive(Clone, clap::ValueEnum)]
enum IngestMode {
    Batch,
    Realtime,
}

#[derive(Subcommand)]
enum Command {
    /// Export a collection (schema + documents) to disk
    Export {
        /// Collection name
        collection: String,
        /// Output directory
        #[arg(short, long, default_value = "bench_data")]
        output: PathBuf,
    },
    /// Import a collection from disk via TCP binary ingest
    Import {
        /// Collection name
        collection: String,
        /// Input directory
        #[arg(short = 'f', long = "from", default_value = "bench_data")]
        input: PathBuf,
        /// Batch size (documents per TCP frame). Larger batches amortize the
        /// per-frame server overhead so the multi-threaded parse/encode
        /// saturates all cores; lower it on memory-constrained hosts.
        #[arg(short, long, default_value = "50000")]
        batch_size: usize,
        /// Ingest mode: batch (full rebuild) or realtime (upserts into the collection's segments)
        #[arg(short, long, default_value = "batch")]
        mode: IngestMode,
    },
    /// Delete documents by ID (realtime mode only)
    Delete {
        /// Collection name
        collection: String,
        /// Document IDs to delete (comma-separated)
        #[arg(long, value_delimiter = ',')]
        ids: Vec<String>,
    },
    /// Export collection to binary file (TQBF format with LZ4 compression)
    ExportBinary {
        /// Collection name
        collection: String,
        /// Output file path
        #[arg(short, long)]
        output: PathBuf,
    },
    /// Import collection from binary file (TQBF format, via HTTP upload)
    ImportBinary {
        /// Collection name
        collection: String,
        /// Binary file path
        #[arg(short = 'f', long = "file")]
        file: PathBuf,
    },
    /// List all collections
    Collections,
    /// Build a segment file (seg_00000000.tqbf) from a docs-only TQBF file (SCHEMA, FIELDS,
    /// RECORDS) with the streaming builder. Runs offline; needs no server. Clients build complete
    /// segments themselves (.NET `TqbfSegmentBuilder`); this is for docs-only files.
    Build {
        /// Docs-only TQBF file
        input: PathBuf,
        /// Output directory
        #[arg(short, long)]
        output: PathBuf,
        /// zstd level of the stored documents
        #[arg(long, default_value = "19")]
        level: i32,
        /// Also store sections that zstd shrinks by 10% or more as zstd frames for transport
        /// (the server restores them on upload)
        #[arg(long)]
        transport: bool,
    },
    /// Upload segment files (a directory from `build`, or one file) as a new collection, then
    /// optionally point an alias at it and drop the collection the alias pointed to before
    Upload {
        /// New collection name
        collection: String,
        /// Segment file, or a directory of seg_*.tqbf files
        #[arg(short = 'f', long = "from")]
        path: PathBuf,
        /// Alias to switch to the new collection
        #[arg(long)]
        alias: Option<String>,
        /// Delete the collection the alias pointed to before
        #[arg(long, requires = "alias")]
        drop_old: bool,
    },
}

fn main() {
    let cli = Cli::parse();

    let key = || -> Result<String, Box<dyn std::error::Error>> {
        cli.api_key.clone().ok_or_else(|| "--api-key (or TORQUE_API_KEY) is required".into())
    };
    let result = match cli.command {
        Command::Build { input, output, level, transport } => {
            cmd_build(&input, &output, level, transport)
        }
        Command::Upload { collection, path, alias, drop_old } => {
            key().and_then(|k| cmd_upload(&cli.url, &k, &collection, &path, alias.as_deref(), drop_old))
        }
        Command::Export { collection, output } => key().and_then(|k| cmd_export(&cli.url, &k, &collection, &output)),
        Command::Import { collection, input, batch_size, mode } => key().and_then(|k| match mode {
            IngestMode::Batch => cmd_import(&cli.url, &k, &cli.tcp_addr, &collection, &input, batch_size),
            IngestMode::Realtime => cmd_import_realtime(&k, &cli.tcp_addr, &collection, &input, batch_size),
        }),
        Command::Delete { collection, ids } => key().and_then(|k| cmd_delete(&k, &cli.tcp_addr, &collection, &ids)),
        Command::ExportBinary { collection, output } => key().and_then(|k| cmd_export_binary(&cli.url, &k, &collection, &output)),
        Command::ImportBinary { collection, file } => key().and_then(|k| cmd_import_binary(&cli.url, &k, &collection, &file)),
        Command::Collections => key().and_then(|k| cmd_collections(&cli.url, &k)),
    };

    if let Err(e) = result {
        eprintln!("Error: {e}");
        std::process::exit(1);
    }
}

// -- HTTP helpers --

fn http_get(url: &str, api_key: &str) -> Result<ureq::Body, Box<dyn std::error::Error>> {
    let resp = ureq::get(url)
        .header("X-TYPESENSE-API-KEY", api_key)
        .call()?;
    Ok(resp.into_body())
}

fn http_delete(url: &str, api_key: &str) -> Result<(), Box<dyn std::error::Error>> {
    match ureq::delete(url)
        .header("X-TYPESENSE-API-KEY", api_key)
        .call()
    {
        Ok(_) => Ok(()),
        Err(ureq::Error::StatusCode(404)) => Ok(()), // not found is fine
        Err(e) => Err(e.into()),
    }
}

fn http_post_json(url: &str, api_key: &str, body: &serde_json::Value) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    let resp = ureq::post(url)
        .header("X-TYPESENSE-API-KEY", api_key)
        .header("Content-Type", "application/json")
        .send_json(body)?;
    let val: serde_json::Value = resp.into_body().read_json()?;
    Ok(val)
}

// -- TCP framing (matches server: u32 BE length prefix) --

fn tcp_write_frame(stream: &mut TcpStream, data: &[u8]) -> Result<(), Box<dyn std::error::Error>> {
    stream.write_all(&(data.len() as u32).to_be_bytes())?;
    stream.write_all(data)?;
    stream.flush()?;
    Ok(())
}

fn tcp_read_frame(stream: &mut TcpStream) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let mut len_buf = [0u8; 4];
    stream.read_exact(&mut len_buf)?;
    let len = u32::from_be_bytes(len_buf) as usize;
    let mut buf = vec![0u8; len];
    stream.read_exact(&mut buf)?;
    Ok(buf)
}

fn tcp_send_and_recv(stream: &mut TcpStream, msg: &ClientMessage) -> Result<ServerMessage, Box<dyn std::error::Error>> {
    let data = serialize_client_message(msg);
    tcp_write_frame(stream, &data)?;
    let resp = tcp_read_frame(stream)?;
    let server_msg = deserialize_server_message(&resp)
        .map_err(|e| format!("protocol error: {e}"))?;
    Ok(server_msg)
}

// -- Commands --

fn cmd_export(
    base_url: &str,
    api_key: &str,
    collection: &str,
    output_dir: &PathBuf,
) -> Result<(), Box<dyn std::error::Error>> {
    std::fs::create_dir_all(output_dir)?;

    // 1. Fetch and save schema
    let schema_url = format!("{base_url}/collections/{collection}");
    let mut schema_body = http_get(&schema_url, api_key)?;
    let schema_json: serde_json::Value = schema_body.read_json()?;
    let schema_path = output_dir.join(format!("{collection}.schema.json"));
    let schema_str = serde_json::to_string_pretty(&schema_json)?;
    std::fs::write(&schema_path, &schema_str)?;
    println!("Schema saved to {}", schema_path.display());

    // 2. Fetch and save documents (streaming JSONL)
    let export_url = format!("{base_url}/collections/{collection}/documents/export");
    let body = http_get(&export_url, api_key)?;
    let docs_path = output_dir.join(format!("{collection}.jsonl"));
    let mut file = std::fs::File::create(&docs_path)?;
    let mut reader = body.into_reader();
    let mut buf = Vec::new();
    reader.read_to_end(&mut buf)?;
    file.write_all(&buf)?;

    let doc_count = buf.iter().filter(|&&b| b == b'\n').count()
        + if !buf.is_empty() && *buf.last().unwrap() != b'\n' { 1 } else { 0 };
    let file_size = buf.len();
    println!(
        "Exported {doc_count} docs to {} ({:.1} MB)",
        docs_path.display(),
        file_size as f64 / 1_048_576.0
    );

    Ok(())
}

fn cmd_import(
    base_url: &str,
    api_key: &str,
    tcp_addr: &str,
    collection: &str,
    input_dir: &PathBuf,
    batch_size: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    let schema_path = input_dir.join(format!("{collection}.schema.json"));
    let docs_path = input_dir.join(format!("{collection}.jsonl"));

    if !schema_path.exists() {
        return Err(format!("Schema file not found: {}", schema_path.display()).into());
    }
    if !docs_path.exists() {
        return Err(format!("JSONL file not found: {}", docs_path.display()).into());
    }

    // 1. Read and parse schema
    let schema_str = std::fs::read_to_string(&schema_path)?;
    let schema_json: serde_json::Value = serde_json::from_str(&schema_str)?;
    let mut schema: Schema = serde_json::from_value(schema_json.clone())?;
    schema.assign_field_ids();

    // 2. Recreate collection via HTTP (delete aliases + collection if exists, then create)
    // First, find and delete any aliases pointing to this collection
    let aliases_url = format!("{base_url}/aliases");
    if let Ok(mut body) = http_get(&aliases_url, api_key) {
        if let Ok(aliases_json) = body.read_json::<serde_json::Value>() {
            if let Some(arr) = aliases_json.get("aliases").and_then(|v| v.as_array()) {
                for alias in arr {
                    if alias.get("collection_name").and_then(|v| v.as_str()) == Some(collection) {
                        if let Some(name) = alias.get("name").and_then(|v| v.as_str()) {
                            let alias_url = format!("{base_url}/aliases/{name}");
                            let _ = http_delete(&alias_url, api_key);
                            println!("Deleted alias '{name}'");
                        }
                    }
                }
            }
        }
    }
    let collection_url = format!("{base_url}/collections/{collection}");
    let _ = http_delete(&collection_url, api_key);
    let collections_url = format!("{base_url}/collections");

    // Build creation payload from schema — only include fields the API expects
    let create_payload = build_create_payload(&schema_json);
    http_post_json(&collections_url, api_key, &create_payload)?;
    println!("Collection '{collection}' created");

    // 3. Count lines for progress bar
    let file = std::fs::File::open(&docs_path)?;
    let line_count = BufReader::new(file).lines().count() as u64;

    // 4. Connect TCP and stream documents
    let mut stream = TcpStream::connect(tcp_addr)
        .map_err(|e| format!("Failed to connect to TCP ingest at {tcp_addr}: {e}"))?;

    // StartIngest
    let resp = tcp_send_and_recv(&mut stream, &ClientMessage::StartIngest {
        collection: collection.to_string(),
        mode: 0,
    })?;
    match &resp {
        ServerMessage::Ack { .. } => {}
        ServerMessage::Error { code, message } => {
            return Err(format!("StartIngest failed (code {code}): {message}").into());
        }
        other => return Err(format!("Unexpected response to StartIngest: {other:?}").into()),
    }

    let pb = ProgressBar::new(line_count);
    pb.set_style(
        ProgressStyle::default_bar()
            .template("{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {pos}/{len} docs ({per_sec}, ETA {eta})")
            .unwrap()
            .progress_chars("=>-"),
    );

    // Multi-threaded ingest: a reader thread streams raw JSONL lines in
    // batch-sized chunks over a bounded channel while the main thread fans the
    // per-document JSON parse across the rayon pool and sends each encoded batch
    // over TCP: a single-threaded parse would be the ceiling, as the server
    // ingests batches an order of magnitude faster. The bounded channel (depth 4) overlaps disk read with parse+send while capping
    // in-flight memory. Chunks arrive in file order, so document ids assigned as
    // base..base+len stay contiguous and deterministic.
    let (tx, rx) = std::sync::mpsc::sync_channel::<Vec<String>>(4);
    let reader_docs_path = docs_path.clone();
    let reader_handle = std::thread::spawn(move || -> std::io::Result<()> {
        let file = std::fs::File::open(&reader_docs_path)?;
        let reader = BufReader::new(file);
        let mut chunk: Vec<String> = Vec::with_capacity(batch_size);
        for line in reader.lines() {
            let line = line?;
            if line.is_empty() {
                continue;
            }
            chunk.push(line);
            if chunk.len() >= batch_size {
                let full = std::mem::replace(&mut chunk, Vec::with_capacity(batch_size));
                if tx.send(full).is_err() {
                    return Ok(()); // consumer went away
                }
            }
        }
        if !chunk.is_empty() {
            let _ = tx.send(chunk);
        }
        Ok(())
    });

    let mut total_docs = 0u64;
    // Count every frame we send. The server replies once per frame, and we must
    // read every reply before closing the socket — see the drain note below.
    let mut frames_sent = 0u64;
    let start = Instant::now();

    for raw in rx {
        let base = total_docs as u32;
        let msg = torque::ingest::encode_batch_parallel(&raw, &schema, base, |line, id| {
            let json: serde_json::Value = serde_json::from_str(line)
                .map_err(|e| format!("Invalid JSON at doc {id}: {e}"))?;
            parse_json_document(&json, &schema, id)
                .map_err(|e| format!("Failed to parse doc {id}: {e}"))
        })?;
        tcp_write_frame(&mut stream, &serialize_client_message(&msg))?;
        frames_sent += 1;
        total_docs += raw.len() as u64;
        pb.set_position(total_docs);
    }

    match reader_handle.join() {
        Ok(Ok(())) => {}
        Ok(Err(e)) => return Err(format!("Failed reading {}: {e}", docs_path.display()).into()),
        Err(_) => return Err("JSONL reader thread panicked".into()),
    }

    // Commit, then block-read exactly one reply per frame we sent (one Ack per
    // DocBatch plus the Commit). Every reply must be consumed before the socket
    // is dropped: closing a TCP socket that still holds unread bytes forces an
    // RST rather than a graceful FIN, which can discard an as-yet-unread Commit
    // frame — the async index build then never starts and the collection is left
    // silently empty. Replies are tiny, so buffering one per batch is cheap.
    tcp_write_frame(&mut stream, &serialize_client_message(&ClientMessage::Commit))?;
    frames_sent += 1;
    let mut commit_docs = 0u64;
    for _ in 0..frames_sent {
        match deserialize_server_message(&tcp_read_frame(&mut stream)?)? {
            ServerMessage::Ack { docs_received, .. } => commit_docs = docs_received,
            ServerMessage::CommitResult { .. } => {}
            ServerMessage::Error { code, message } => {
                pb.finish_and_clear();
                return Err(format!("Ingest error (code {code}): {message}").into());
            }
            other => {
                pb.finish_and_clear();
                return Err(format!("Unexpected ingest response: {other:?}").into());
            }
        }
    }
    pb.finish_and_clear();
    println!(
        "Imported {commit_docs} docs in {:.1}s (index building async on server)",
        start.elapsed().as_secs_f64()
    );

    Ok(())
}

fn build_create_payload(schema_json: &serde_json::Value) -> serde_json::Value {
    let mut payload = serde_json::Map::new();
    if let Some(name) = schema_json.get("name") {
        payload.insert("name".to_string(), name.clone());
    }
    if let Some(fields) = schema_json.get("fields") {
        payload.insert("fields".to_string(), fields.clone());
    }
    if let Some(v) = schema_json.get("default_sorting_field") {
        payload.insert("default_sorting_field".to_string(), v.clone());
    }
    if let Some(v) = schema_json.get("enable_nested_fields") {
        payload.insert("enable_nested_fields".to_string(), v.clone());
    }
    if let Some(v) = schema_json.get("symbols_to_index") {
        payload.insert("symbols_to_index".to_string(), v.clone());
    }
    if let Some(v) = schema_json.get("token_separators") {
        payload.insert("token_separators".to_string(), v.clone());
    }
    if let Some(v) = schema_json.get("default_language") {
        payload.insert("default_language".to_string(), v.clone());
    }
    if let Some(v) = schema_json.get("persistence_enabled") {
        payload.insert("persistence_enabled".to_string(), v.clone());
    }
    serde_json::Value::Object(payload)
}

fn cmd_import_realtime(
    _api_key: &str,
    tcp_addr: &str,
    collection: &str,
    input_dir: &PathBuf,
    batch_size: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    let schema_path = input_dir.join(format!("{collection}.schema.json"));
    let docs_path = input_dir.join(format!("{collection}.jsonl"));

    if !schema_path.exists() {
        return Err(format!("Schema file not found: {}", schema_path.display()).into());
    }
    if !docs_path.exists() {
        return Err(format!("JSONL file not found: {}", docs_path.display()).into());
    }

    // Parse schema for document encoding
    let schema_str = std::fs::read_to_string(&schema_path)?;
    let schema_json: serde_json::Value = serde_json::from_str(&schema_str)?;
    let mut schema: Schema = serde_json::from_value(schema_json)?;
    schema.assign_field_ids();

    // Count lines for progress bar
    let file = std::fs::File::open(&docs_path)?;
    let line_count = BufReader::new(file).lines().count() as u64;

    let pb = ProgressBar::new(line_count);
    pb.set_style(
        ProgressStyle::default_bar()
            .template("{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {pos}/{len} docs ({per_sec}, ETA {eta})")
            .unwrap()
            .progress_chars("=>-"),
    );

    // Connect TCP
    let mut stream = TcpStream::connect(tcp_addr)
        .map_err(|e| format!("Failed to connect to TCP ingest at {tcp_addr}: {e}"))?;

    let file = std::fs::File::open(&docs_path)?;
    let reader = BufReader::new(file);
    let mut batch_docs = Vec::new();
    let mut total_docs = 0u64;
    let mut doc_id = 0u32;
    let start = Instant::now();

    for line in reader.lines() {
        let line = line?;
        if line.is_empty() {
            continue;
        }
        let json: serde_json::Value = serde_json::from_str(&line)
            .map_err(|e| format!("Invalid JSON at doc {doc_id}: {e}"))?;
        let doc = parse_json_document(&json, &schema, doc_id)
            .map_err(|e| format!("Failed to parse doc {doc_id}: {e}"))?;
        batch_docs.push(doc);
        doc_id += 1;

        if batch_docs.len() >= batch_size {
            send_upsert_batch(&mut stream, &batch_docs, &schema, collection)?;
            total_docs += batch_docs.len() as u64;
            pb.set_position(total_docs);
            batch_docs.clear();
        }
    }

    if !batch_docs.is_empty() {
        send_upsert_batch(&mut stream, &batch_docs, &schema, collection)?;
        total_docs += batch_docs.len() as u64;
        pb.set_position(total_docs);
    }

    pb.finish_and_clear();
    let elapsed = start.elapsed();
    println!(
        "Upserted {} docs in {:.1}s ({:.0} docs/s) via realtime mode",
        total_docs,
        elapsed.as_secs_f64(),
        total_docs as f64 / elapsed.as_secs_f64(),
    );

    Ok(())
}

fn send_upsert_batch(
    stream: &mut TcpStream,
    batch: &[torque::schema::Document],
    schema: &Schema,
    collection: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut payload = Vec::new();
    for doc in batch {
        payload.extend(serialize_document(doc, schema));
    }
    let msg = ClientMessage::Upsert {
        collection: collection.to_string(),
        action: 0,
        doc_count: batch.len() as u32,
        payload,
    };
    let resp = tcp_send_and_recv(stream, &msg)?;
    match resp {
        ServerMessage::Ack { .. } => Ok(()),
        ServerMessage::Backpressure { retry_after_ms } => {
            std::thread::sleep(std::time::Duration::from_millis(retry_after_ms as u64));
            // Retry once
            let mut payload = Vec::new();
            for doc in batch {
                payload.extend(serialize_document(doc, schema));
            }
            let msg = ClientMessage::Upsert {
                collection: collection.to_string(),
                action: 0,
                doc_count: batch.len() as u32,
                payload,
            };
            let resp = tcp_send_and_recv(stream, &msg)?;
            match resp {
                ServerMessage::Ack { .. } => Ok(()),
                other => Err(format!("Upsert retry failed: {other:?}").into()),
            }
        }
        ServerMessage::Error { code, message } => {
            Err(format!("Upsert error (code {code}): {message}").into())
        }
        other => Err(format!("Unexpected upsert response: {other:?}").into()),
    }
}

fn cmd_delete(
    _api_key: &str,
    tcp_addr: &str,
    collection: &str,
    ids: &[String],
) -> Result<(), Box<dyn std::error::Error>> {
    if ids.is_empty() {
        return Err("No IDs specified. Use --ids id1,id2,...".into());
    }

    let mut stream = TcpStream::connect(tcp_addr)
        .map_err(|e| format!("Failed to connect to TCP ingest at {tcp_addr}: {e}"))?;

    let payload = encode_delete_ids(ids);
    let msg = ClientMessage::Upsert {
        collection: collection.to_string(),
        action: 1,
        doc_count: ids.len() as u32,
        payload,
    };
    let resp = tcp_send_and_recv(&mut stream, &msg)?;
    match resp {
        ServerMessage::Ack { docs_received, .. } => {
            println!("Deleted {} document(s) from '{collection}'", docs_received);
            Ok(())
        }
        ServerMessage::Error { code, message } => {
            Err(format!("Delete error (code {code}): {message}").into())
        }
        other => Err(format!("Unexpected response: {other:?}").into()),
    }
}

fn cmd_export_binary(
    base_url: &str,
    api_key: &str,
    collection: &str,
    output: &PathBuf,
) -> Result<(), Box<dyn std::error::Error>> {
    // 1. Fetch schema
    let schema_url = format!("{base_url}/collections/{collection}");
    let mut schema_body = http_get(&schema_url, api_key)?;
    let schema_json: serde_json::Value = schema_body.read_json()?;
    let mut schema: Schema = serde_json::from_value(schema_json)?;
    schema.assign_field_ids();

    // 2. Export documents as JSONL
    println!("Exporting documents from '{collection}'...");
    let export_url = format!("{base_url}/collections/{collection}/documents/export");
    let body = http_get(&export_url, api_key)?;
    let mut reader = body.into_reader();
    let mut jsonl_buf = Vec::new();
    reader.read_to_end(&mut jsonl_buf)?;
    let jsonl = String::from_utf8_lossy(&jsonl_buf);

    // 3. Encode to binary
    let mut payload = Vec::new();
    let mut doc_count = 0u32;
    let mut doc_id = 0u32;

    let pb = ProgressBar::new_spinner();
    pb.set_style(ProgressStyle::default_spinner().template("{spinner:.green} {pos} docs encoded").unwrap());

    for line in jsonl.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let json: serde_json::Value = serde_json::from_str(line)?;
        let doc = parse_json_document(&json, &schema, doc_id)?;
        payload.extend(serialize_document(&doc, &schema));
        doc_count += 1;
        doc_id += 1;
        if doc_count % 10000 == 0 {
            pb.set_position(doc_count as u64);
        }
    }
    pb.finish_and_clear();

    // 4. LZ4 compress
    let uncompressed_size = payload.len() as u64;
    let compressed = lz4_flex::compress_prepend_size(&payload);
    let ratio = payload.len() as f64 / compressed.len() as f64;

    // 5. Write TQBF v2 file (24-byte header). The u64 uncompressed_size lets
    // the server size the decompression Vec exactly once.
    let mut file = std::fs::File::create(output)?;
    file.write_all(b"TQBF")?;                                       // magic
    file.write_all(&2u32.to_le_bytes())?;                            // version: 2
    file.write_all(&doc_count.to_le_bytes())?;                       // doc_count
    file.write_all(&1u32.to_le_bytes())?;                            // flags: LZ4
    file.write_all(&uncompressed_size.to_le_bytes())?;               // NEW: uncompressed size
    file.write_all(&compressed)?;                                     // payload

    let file_size = 24 + compressed.len();
    println!(
        "Exported {} docs to {} ({:.1} MB, {:.1}x compression, raw {:.1} MB)",
        doc_count,
        output.display(),
        file_size as f64 / 1_048_576.0,
        ratio,
        payload.len() as f64 / 1_048_576.0,
    );
    Ok(())
}

fn cmd_import_binary(
    base_url: &str,
    api_key: &str,
    collection: &str,
    file: &PathBuf,
) -> Result<(), Box<dyn std::error::Error>> {
    if !file.exists() {
        return Err(format!("File not found: {}", file.display()).into());
    }

    let file_data = std::fs::read(file)?;
    let file_size = file_data.len();
    println!(
        "Uploading {} ({:.1} MB) to '{collection}'...",
        file.display(),
        file_size as f64 / 1_048_576.0,
    );

    let url = format!("{base_url}/collections/{collection}/documents/import-binary");
    let t0 = Instant::now();
    let resp = ureq::post(&url)
        .header("X-TYPESENSE-API-KEY", api_key)
        .header("Content-Type", "application/octet-stream")
        .send(file_data.as_slice())?;
    let elapsed = t0.elapsed();

    let result: serde_json::Value = resp.into_body().read_json()?;
    let num_imported = result["num_imported"].as_u64().unwrap_or(0);
    let server_ms = result["elapsed_ms"].as_f64().unwrap_or(0.0);

    println!(
        "Imported {} docs in {:.1}s (upload {:.1}s, server {:.0}ms, {:.0} docs/s)",
        num_imported,
        elapsed.as_secs_f64(),
        elapsed.as_secs_f64() - server_ms / 1000.0,
        server_ms,
        num_imported as f64 / elapsed.as_secs_f64(),
    );
    Ok(())
}

fn cmd_build(
    input: &std::path::Path,
    output: &std::path::Path,
    level: i32,
    transport: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    use torque::ingest::segment_file::{build, BuildOptions};
    let t0 = Instant::now();
    let report = build(input, output, &BuildOptions { docs_level: level })?;
    for r in report.rejected.iter().take(20) {
        eprintln!("rejected document {} (id `{}`): {}", r.index, r.id, r.error);
    }
    let mut total = 0u64;
    for (path, docs) in &report.segments {
        if transport {
            torque::tqbf::container::compress_file_for_transport(path, 19, 0.1)?;
        }
        let bytes = std::fs::metadata(path)?.len();
        total += bytes;
        println!("{}  {docs} docs  {:.1} MB", path.display(), bytes as f64 / 1_048_576.0);
    }
    let docs: u64 = report.segments.iter().map(|s| u64::from(s.1)).sum();
    println!(
        "Built {docs} documents into {} segment files ({:.1} MB) in {:.1}s; {} rejected",
        report.segments.len(),
        total as f64 / 1_048_576.0,
        t0.elapsed().as_secs_f64(),
        report.rejected.len(),
    );
    if !report.rejected.is_empty() {
        return Err(format!("{} documents rejected", report.rejected.len()).into());
    }
    Ok(())
}

/// PUT with a body, returning (status, body) for every status so server messages reach the user.
fn http_put_raw(url: &str, api_key: &str, body: impl ureq::AsSendBody, content_type: &str) -> Result<(u16, String), Box<dyn std::error::Error>> {
    let agent: ureq::Agent = ureq::Agent::config_builder().http_status_as_error(false).build().into();
    let resp = agent
        .put(url)
        .header("X-TYPESENSE-API-KEY", api_key)
        .header("Content-Type", content_type)
        .send(body)?;
    let status = resp.status().as_u16();
    let text = resp.into_body().read_to_string()?;
    Ok((status, text))
}

fn cmd_upload(
    base_url: &str,
    api_key: &str,
    collection: &str,
    path: &std::path::Path,
    alias: Option<&str>,
    drop_old: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let files: Vec<PathBuf> = if path.is_dir() {
        let mut v: Vec<PathBuf> = std::fs::read_dir(path)?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("seg_") && n.ends_with(".tqbf")))
            .collect();
        v.sort();
        v
    } else {
        vec![path.to_path_buf()]
    };
    if files.is_empty() {
        return Err(format!("no seg_*.tqbf files in {}", path.display()).into());
    }
    let t0 = Instant::now();
    let parts = files.len();
    let mut last = String::new();
    for (i, f) in files.iter().enumerate() {
        let size = std::fs::metadata(f)?.len();
        let url = format!("{base_url}/collections/{collection}/segments?part={i}&parts={parts}");
        let t = Instant::now();
        let (status, body) = http_put_raw(&url, api_key, &std::fs::File::open(f)?, "application/octet-stream")?;
        if status >= 300 {
            return Err(format!("upload of {} failed ({status}): {body}", f.display()).into());
        }
        println!(
            "Uploaded {} ({:.1} MB) in {:.1}s",
            f.display(),
            size as f64 / 1_048_576.0,
            t.elapsed().as_secs_f64()
        );
        last = body;
    }
    let created: serde_json::Value = serde_json::from_str(&last)?;
    println!(
        "Collection '{collection}' created with {} documents in {:.1}s",
        created["num_documents"].as_u64().unwrap_or(0),
        t0.elapsed().as_secs_f64()
    );
    if let Some(alias) = alias {
        let previous = match http_get(&format!("{base_url}/aliases/{alias}"), api_key) {
            Ok(mut b) => b.read_json::<serde_json::Value>().ok().and_then(|v| v["collection_name"].as_str().map(str::to_string)),
            Err(_) => None,
        };
        let (status, body) = http_put_raw(
            &format!("{base_url}/aliases/{alias}"),
            api_key,
            serde_json::json!({ "collection_name": collection }).to_string(),
            "application/json",
        )?;
        if status >= 300 {
            return Err(format!("alias update failed ({status}): {body}").into());
        }
        println!("Alias '{alias}' -> '{collection}' (was {})", previous.as_deref().unwrap_or("unset"));
        if drop_old {
            if let Some(old) = previous.filter(|p| p != collection) {
                http_delete(&format!("{base_url}/collections/{old}"), api_key)?;
                println!("Dropped '{old}'");
            }
        }
    }
    Ok(())
}

fn cmd_collections(
    base_url: &str,
    api_key: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let url = format!("{base_url}/collections");
    let mut body = http_get(&url, api_key)?;
    let collections: serde_json::Value = body.read_json()?;

    let arr = collections.as_array().ok_or("Expected array response")?;
    if arr.is_empty() {
        println!("No collections found.");
        return Ok(());
    }

    println!("{:<50} {:>10} {:>8}", "NAME", "DOCS", "FIELDS");
    println!("{}", "-".repeat(70));
    for coll in arr {
        let name = coll.get("name").and_then(|v| v.as_str()).unwrap_or("?");
        let num_docs = coll.get("num_documents").and_then(|v| v.as_u64()).unwrap_or(0);
        let num_fields = coll.get("fields").and_then(|v| v.as_array()).map(|a| a.len()).unwrap_or(0);
        println!("{:<50} {:>10} {:>8}", name, num_docs, num_fields);
    }

    Ok(())
}
