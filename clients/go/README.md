# Torque Go client

The Go client for [Torque](https://truespar.com/torque/docs), an in-memory search engine with a Typesense-compatible API.

- Standard library only, no dependencies
- Safe for concurrent use; connections are pooled
- `context.Context` on every request, for cancellation and timeouts
- **TCP ingest** (`IngestClient`) for streaming documents
- **Binary import files** (`BinaryWriter`) for bulk loads
- Several nodes, with failover and retries
- Go 1.23+

## Installation

```bash
go get github.com/truespar/torque/clients/go
```

## Quick start

```go
package main

import (
    "context"
    "fmt"
    torque "github.com/truespar/torque/clients/go"
)

func main() {
    client := torque.NewHttpClient("http://localhost:8108", &torque.Options{
        ApiKey: "your-key",
    })
    defer client.Close()

    ctx := context.Background()
    result, err := client.Search(ctx, "products", map[string]string{
        "q":        "laptop",
        "query_by": "title",
    })
    if err != nil {
        panic(err)
    }
    for _, hit := range result.Hits {
        fmt.Println(hit.Document["title"])
    }
}
```

## Several nodes

```go
client := torque.NewHttpClient("", &torque.Options{
    ApiKey: "your-key",
    Nodes: []torque.NodeConfig{
        {Host: "node1.example.com", Port: 8108, Protocol: "https"},
        {Host: "node2.example.com", Port: 8108, Protocol: "https"},
    },
    NumRetries: 3,
})
```

## Collections

```go
schema := map[string]any{
    "name": "products",
    "fields": []any{
        map[string]any{"name": "title", "type": "string"},
        map[string]any{"name": "price", "type": "float", "sort": true},
        map[string]any{"name": "brand", "type": "string", "facet": true},
    },
}
_, err := client.CreateCollection(ctx, schema)
```

## TCP ingest

```go
schema := []torque.FieldDef{
    {Name: "title", Type: "string"},
    {Name: "price", Type: "float"},
}

tcp := &torque.IngestClient{}
if err := tcp.Connect("localhost", 8119); err != nil {
    panic(err)
}
defer tcp.Close()

tcp.StartIngest("products")
tcp.SendBatch(documents, schema)
tcp.Commit()
```

## Binary import file

```go
w := torque.NewBinaryWriter(schema, "data.tqbf")
for _, doc := range source {
    w.Write(doc)
}
w.Finalize()

_, err := client.ImportBinary(ctx, "products", "data.tqbf")
```

## Documentation

- [Torque documentation](https://truespar.com/torque/docs)

## License

MIT OR Apache-2.0, at your option. See [LICENSE](LICENSE). Copyright 2025-2026 The Intelligence Company AB (publ).
