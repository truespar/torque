# Torque

**Publish large searchable datasets efficiently.**

Torque is a search engine written in Rust for large datasets that you rebuild from an existing database. Build a complete search index near your source data, upload it as compact segment files, and switch a collection alias. Torque serves the files through a Typesense-compatible API, with text search, filters, facets, sorting, geo search and name matching.

Its strongest fit is a dataset refreshed on a schedule and searched throughout the day: company registers, supplier directories, public records, product catalogs and other reference data.

## Why build the index separately?

A full reindex through a document API asks the search server to receive documents, parse them and build their indexes while it also handles searches. When you replace a large collection regularly, that work can require hours of reindexing if the collection is huge.

Torque lets you move the build to the machine that has the data, or to a separate build machine. The result is a set of immutable segment files containing both the stored documents and their indexes. The server verifies the uploaded files and reads their indexes directly through memory mapping.

```text
Source database -> Build segment files -> Upload and validate -> Switch alias -> Serve searches
```

- **Separate build and serving resources.** Run indexing and compression away from the search server. The server still receives and verifies the files, but does no indexing for a prebuilt collection.
- **Publish complete versions.** Prepare and query a new collection while the previous one continues serving users. Switch the alias when it is ready, and retain the previous collection if you need a rollback.
- **Reuse the built index at restart.** Map the existing files instead of rebuilding the index from documents. Frequently accessed pages stay in the operating system's cache; cold pages are read from disk. Performance still depends on RAM, storage and the queries you run.

## When Torque fits

Torque is a strong fit when searches greatly outnumber changes, your application can use scheduled snapshots, and you can rebuild the search data from an authoritative source.

- **Company and people directories:** search names across spelling variants, combine matches with structured filters, and explain how a name matched.
- **Catalogs and reference data:** publish a complete daily or periodic update, then serve repeated searches with facets, sorting and geographic filters.
- **Public records and research datasets:** turn a prepared dataset into a searchable collection without rebuilding its index on the serving machine.

For example, a company-information service can build tomorrow's collection overnight, validate its records and searches, then switch the public alias in the morning. The current collection remains available throughout the build.

Torque also supports realtime writes through a write-ahead log and background segment merges. If continuous, intensive updates or built-in replicated high availability are central to your workload, Typesense is the more established choice. Torque currently has no built-in distributed replication or sharding.

## Features

- **Memory-mapped serving** from compact segment files persisted on disk
- **Typesense v30.1 API compatibility** for existing clients, with the search differences described below
- **Text and vector search**: BM25F, typo tolerance, HNSW with RaBitQ quantization, hybrid search with RRF
- **Name matching** for people and companies, by passport spelling and by sound
- **Four ingestion paths**: HTTP JSONL, the TCP ingest protocol, binary import files, segment files built on the client
- **Search with a language model**: conversational search, query translation from natural language, auto-embedding through OpenAI-compatible endpoints
- **MCP server** at `/api/mcp`
- **Studio**: search editor, schema browser, benchmark panel, and a Search Assistant on your own model
- **GPU acceleration** with NVIDIA CUDA, built from source
- **TLS**, Prometheus metrics, runs as a Windows service
- **Clients** for .NET, Python, Node.js, Go and Java

## How Torque differs from Typesense

**Building and publishing.** Both engines support collection aliases, so an application can switch to a new collection without changing its queries. Torque additionally accepts finished indexes built outside the server, using the native builder in the .NET client or `torque-admin build`. HTTP JSONL and the TCP ingest protocol remain available for documents you want the server to index.

**Storage and memory.** Typesense stores documents in RocksDB and holds its search index in memory. Torque stores documents and indexes together in immutable segment files and reads their compact structures in place. The operating system manages which file pages are resident in memory. This avoids loading the entire index into heap structures, but does not guarantee lower memory use or faster searches for every workload.

**Realtime writes.** Torque indexes each write group as a small segment, hides replaced documents with deletion masks, and merges segments in the background. A write-ahead log makes acknowledged writes durable. This path supports updates between full rebuilds, but Typesense and RocksDB have a longer operational history. See [the realtime write path](docs/realtime-write-path.md) for the design and its verification.

**Search behavior.** Torque implements its own text search: BM25F scoring, typo matching through a Levenshtein automaton, and prefix completions from each field's term dictionary. API compatibility does not mean identical results or ranking. In particular:

- `text_match` values and result ordering are Torque's own; test relevance with your application's queries before migrating.
- Word positions are not indexed. A `"phrase"` is checked against stored text and can cost more than a plain search.
- `prioritize_token_position` and a field's or synonym's `locale` have no effect.
- Unknown search parameters are ignored where Typesense returns HTTP 400.

**Code and license.** Torque is MIT OR Apache-2.0 and shares no code with Typesense, which is GPL-3.0. Compatibility comes from Typesense's public documentation and from testing against a running server.

## Getting started

### Start the server

```bash
# Default start
torque-server --api-key YOUR_KEY

# With TLS
torque-server --api-key YOUR_KEY --tls-cert cert.pem --tls-key key.pem

# Other ports
torque-server --api-key YOUR_KEY --api-addr 0.0.0.0:8108 --ingest-addr 127.0.0.1:8119
```

The HTTP API and Studio listen on port **8108**. The TCP ingest protocol listens on port **8109** (pass `--ingest-addr 127.0.0.1:8119` if another service holds 8109). Without `--api-key` the server generates a key and prints it at start.

### Open Studio

Open [http://localhost:8108](http://localhost:8108) in your browser and log in with the API key from the server output.

### Search

```bash
curl -H "X-TYPESENSE-API-KEY: YOUR_KEY" \
  "http://localhost:8108/collections/products/documents/search?q=shoes&query_by=title"
```

### Docker

```bash
docker build -t torque .
docker run -p 8108:8108 -p 8109:8109 -v torque-data:/data torque
```

The container prints its generated API key at start (`docker logs`); set `TORQUE_API_KEY` for a key of your own.

## Building from source

```bash
# Server and torque-admin, with TLS and the MCP endpoint
cargo build --release --features tls,mcp

# Rebuild Studio after changing studio/ (the server embeds studio/dist)
cd studio && npm ci && npx vite build

# The native segment builder the .NET client packs
pwsh ffi/torque-build/build_native.ps1
```

## Included tools

| Binary | Description |
|--------|-------------|
| `torque-server` | The server: HTTP API, TCP ingest protocol, Studio |
| `torque-admin` | Collections: list, export, import (batch or realtime), delete; binary import files: export and import; segment files: build and upload |

## Ingestion paths

| Path | For |
|------|-----|
| HTTP JSONL | Small updates; Typesense clients |
| TCP ingest protocol | Streaming from an application, batch or realtime |
| Binary import file | Bulk loads: a compressed file of typed documents, uploaded in one request |
| Segment files | Whole collections built on the client; the server installs them without indexing |

For scheduled rebuilds, use segment files. The offline builder currently requires an explicit schema and does not build vector fields, generate embeddings, or discover dynamic fields. Use the server's HTTP document import for those features. The .NET client can build segments directly from application objects with `TqbfSegmentBuilder`, or write a docs-only file with `TqbfDocsWriter` for `torque-admin build`.

```bash
# HTTP JSONL (Typesense-compatible)
curl -X POST -H "X-TYPESENSE-API-KEY: KEY" \
  "http://localhost:8108/collections/products/documents/import?action=upsert" \
  --data-binary @documents.jsonl

# torque-admin over the TCP ingest protocol
torque-admin --api-key KEY import my_collection

# torque-admin binary import file
torque-admin --api-key KEY export-binary my_collection -o data.tqbf
torque-admin --api-key KEY import-binary my_collection -f data.tqbf

# Build a docs-only file from TqbfDocsWriter into finished segments,
# then upload as a new collection and switch the alias
torque-admin build docs.tqbf -o out/
torque-admin --api-key KEY upload my_collection_20261107 -f out/ --alias my_collection
```

The upload example retains the previous collection so you can switch the alias back if needed. Add `--drop-old` to remove the previous collection after switching.

Binary import files, docs-only build inputs and finished segments share the `.tqbf` extension but are not interchangeable. Files from `TorqueBinaryWriter` or `export-binary` go through the binary import path and are indexed on the server. `torque-admin build` takes a docs-only file from `TqbfDocsWriter` and produces finished segments that the server can serve without indexing.

## Clients

Every client covers the HTTP API, the TCP ingest protocol (`TorqueIngestClient`) and binary import files (`TorqueBinaryWriter`). The .NET client also builds segment files (`TqbfSegmentBuilder`).

| Language | Package | Install |
|---|---|---|
| .NET 10+ | `Torque.Http` | `dotnet add package Torque.Http` |
| Python 3.10+ | `torque-http` | `pip install torque-http` |
| Node.js 20+ | `@truespar/torque-http` | `npm install @truespar/torque-http` |
| Java 21+ | `com.truespar:torque-http` | Maven / Gradle dependency |
| Go 1.23+ | `github.com/truespar/torque/clients/go` | `go get github.com/truespar/torque/clients/go` |

Each client's `README.md` under `clients/` has examples in its language.

## Configuration

Torque reads CLI arguments, environment variables (`TORQUE_*`) and a TOML file. Copy `torque.example.toml` to `torque.toml` and edit it.

**Precedence:** CLI arguments > environment variables > config file > built-in defaults

### Key settings

| Environment variable | CLI flag | Default | Description |
|---------------------|----------|---------|-------------|
| `TORQUE_API_KEY` | `--api-key` | *(generated)* | Bootstrap API key (prefix `tq_` when generated) |
| `TORQUE_API_ADDR` | `--api-addr` | `0.0.0.0:8108` | HTTP API and Studio address |
| `TORQUE_INGEST_ADDR` | `--ingest-addr` | `0.0.0.0:8109` | TCP ingest protocol address |
| `TORQUE_DATA_DIR` | `--data-dir` | `./data` | Data directory |
| `TORQUE_MAX_MEMORY_PERCENT` | `--max-memory-percent` | `90` | Memory ceiling as a share of system RAM |
| `TORQUE_TLS_CERT` | `--tls-cert` | *(off)* | TLS certificate (PEM) |
| `TORQUE_TLS_KEY` | `--tls-key` | *(off)* | TLS private key (PEM) |
| `TORQUE_SLOW_QUERY_MS` | `--slow-query-ms` | `0` | Slow query logging threshold |
| `TORQUE_LOG_DIR` | `--log-dir` | *(stdout)* | Daily rolling log files |
| `TORQUE_AI_PROVIDER` | | *(off)* | Studio's Search Assistant: `anthropic` or `openai` (any OpenAI-compatible endpoint), with `TORQUE_AI_API_KEY` |

`torque.example.toml` lists every option, with CORS, the GPU memory budget, compaction and the query timeout among them.

### Search Assistant

Studio's assistant writes and fixes searches for the open collection, on a model you choose: the Claude API or any OpenAI-compatible endpoint, a local model included. It is off until you set `ai_provider`. The provider's key stays on the server. The assistant sees the collection's schema and never the documents.

## Data on disk

A collection is a directory of segment files and a manifest under the data directory; a realtime collection adds a write-ahead log. A write is on disk before it is acknowledged, a save writes only what changed, and every file replace is atomic (temporary file, sync, rename). At start the server maps the segment files and replays the log.

## GPU acceleration

Build with `--features cuda` for NVIDIA CUDA; it needs the CUDA toolkit. Without a GPU, or without the feature, Torque runs on the CPU.

## Documentation

| Document | Description |
|---|---|
| [Torque documentation](https://truespar.com/torque/docs) | Reference: server, API, clients, benchmarks |
| [Studio](http://localhost:8108) | Search editor, schema browser, benchmark panel |
| [`RELEASE_NOTES.md`](RELEASE_NOTES.md) | Version history |
| [`docs/tqbf-segment-format.md`](docs/tqbf-segment-format.md) | The segment file format |
| [`docs/realtime-write-path.md`](docs/realtime-write-path.md) | How realtime collections take writes |
| [`torque.example.toml`](torque.example.toml) | Every option, with comments |
| [`CONTRIBUTING.md`](CONTRIBUTING.md) | How to build, test and send a change |
| [`SECURITY.md`](SECURITY.md) | How to report a security problem |

## License

Torque is licensed under either of the [MIT license](LICENSE-MIT) or the [Apache License 2.0](LICENSE-APACHE), at your option. Third-party components remain under their own licenses, listed in [THIRD-PARTY-NOTICES](THIRD-PARTY-NOTICES).

Copyright 2025-2026 The Intelligence Company AB (publ). Torque is developed by Truespar, the research arm of The Intelligence Company AB (publ).
