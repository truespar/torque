# Torque.Http

The .NET client for [Torque](https://truespar.com/torque/docs), an in-memory search engine with a Typesense-compatible API.

- No dependencies beyond `System.Net.Http` and `System.Text.Json`; source-generated JSON
- **HTTP API**: search, collections, documents, keys, aliases
- **TCP ingest** (`TorqueIngestClient`) for streaming documents
- **Binary import files** (`TorqueBinaryWriter`) for bulk loads
- **Segment files** built in your process (`TqbfSegmentBuilder`)
- Several nodes, with failover and retries
- .NET 10+

## Installation

```bash
dotnet add package Torque.Http
```

## Quick start

```csharp
using Torque.Http;
using Torque.Http.Models;
using Torque.Http.Search;

var client = new TorqueHttpClient(new TorqueHttpClientOptions
{
    ApiKey = "your-key",
    BaseUrl = "http://localhost:8108",
});

var result = await client.SearchAsync(
    "products",
    new SearchBuilder()
        .Query("laptop")
        .QueryBy("title")
        .FilterBy("price:>100")
        .Page(1, 20));

foreach (var hit in result.Hits)
{
    Console.WriteLine(hit.Document["title"]);
}
```

## Typed search

```csharp
public record Product(string Id, string Title, double Price, string Brand);

var result = await client.SearchAsync<Product>(
    "products",
    new SearchParameters { Q = "laptop", QueryBy = "title" });

foreach (var hit in result.Hits)
{
    Console.WriteLine($"{hit.Document.Title}: ${hit.Document.Price}");
}
```

## Several nodes

```csharp
var client = new TorqueHttpClient(new TorqueHttpClientOptions
{
    ApiKey = "your-key",
    Nodes =
    [
        new Uri("https://node1.example.com:8108"),
        new Uri("https://node2.example.com:8108"),
    ],
    MaxRetries = 3,
    RetryBackoff = TimeSpan.FromMilliseconds(100),
});
```

## Collections and documents

```csharp
var schema = new CollectionSchema
{
    Name = "products",
    Fields =
    [
        new() { Name = "title", Type = TorqueFieldType.String },
        new() { Name = "price", Type = TorqueFieldType.Float, Sort = true },
        new() { Name = "brand", Type = TorqueFieldType.String, Facet = true },
    ],
};
await client.Collections.CreateAsync(schema);

await client.Documents.CreateAsync("products", new Dictionary<string, object?>
{
    ["id"] = "1",
    ["title"] = "Laptop Pro",
    ["price"] = 1299.99,
    ["brand"] = "Acme",
});
```

## TCP ingest

`TorqueDocumentBuilder` gives each field its typed value, so the server stores exactly what you sent.

```csharp
using Torque.Http.Tcp;
using Torque.Http.Models;

var docs = new List<TorqueDocument>
{
    new TorqueDocumentBuilder()
        .SetId("1")
        .Set("title", "Laptop Pro")
        .Set("price", 1299.99)
        .Set("brand", "Acme")
        .Build(schema),
    // ...
};

await using var ingest = new TorqueIngestClient(new TorqueIngestOptions
{
    Host = "localhost",
    Port = 8109,
});
await ingest.ConnectAsync();
await ingest.StartIngestAsync("products");
await ingest.SendBatchAsync(docs, schema);
await ingest.CommitAsync();
```

## Binary import file

Write the documents to a compressed file, then upload it in one request; the writer uploads through the `HttpClient` you pass:

```csharp
using Torque.Http.Tcp;

await using var writer = new TorqueBinaryWriter(schema, "data.tqbf");
foreach (var doc in docs)
    writer.Write(doc);
await writer.FinalizeAsync();

using var http = new HttpClient { BaseAddress = new Uri("http://localhost:8108") };
var result = await writer.UploadAsync(http, "products", "your-key");
Console.WriteLine($"Imported {result.NumImported} documents");
```

## Segment files built in your process

For collections rebuilt in full (nightly), build the finished segment file yourself and upload it; the
server maps it without decoding or indexing anything. `TqbfSegmentBuilder` runs the server's own
indexing code (the native `torque_build` library, shipped in the package) on your machine. Records
go in per run: a run is a stretch of consecutive documents added on one thread; runs are laid out in
key order, so number them by the data they cover and the file is the same whatever the timing.

```csharp
using Torque.Http.Tqbf;

using var builder = new TqbfSegmentBuilder(schemaJson);
var encoder = new TqbfNewtonsoftEncoder(jsonSettings);   // Torque.Http.Newtonsoft package
Parallel.For(0, ranges.Length, i =>
{
    using var run = builder.BeginRun(i);
    foreach (var document in Load(ranges[i]))
        encoder.Add(run, document);                      // or run.AddJson(jsonLine)
    run.End();
});
var report = builder.Finish("companies.tqbf");
await client.Collections.UploadSegmentsAsync("companies_20261017", ["companies.tqbf"]);
```

`TqbfNewtonsoftEncoder` writes each object as the record its Newtonsoft JSON (with your settings)
would give, without writing JSON. Frozen data spills to a temp file, so memory does not grow with the
collection.

## Documentation

- [Torque documentation](https://truespar.com/torque/docs)

## License

MIT OR Apache-2.0, at your option. See [LICENSE](LICENSE). Copyright 2025-2026 The Intelligence Company AB (publ).
