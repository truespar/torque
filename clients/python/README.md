# torque-http

The Python client for [Torque](https://truespar.com/torque/docs), an in-memory search engine with a Typesense-compatible API.

- Standard library only, no dependencies
- **HTTP API**: search, collections, documents, keys, aliases
- **TCP ingest** (`TorqueIngestClient`) for streaming documents
- **Binary import files** (`TorqueBinaryFileWriter`) for bulk loads
- Several nodes, with failover and retries
- Typed search parameters and response models
- Python 3.10+

## Installation

```bash
pip install torque-http
```

## Quick start

```python
from torque_http import TorqueHttpClient, SearchParameters

client = TorqueHttpClient("http://localhost:8108", api_key="your-key")

# Search
results = client.search("products", q="laptop", query_by="title")
for hit in results.hits:
    print(hit.document["title"])

# Typed parameters
params = SearchParameters(
    q="laptop",
    query_by="title,description",
    filter_by="price:>100",
    per_page=20,
)
results = client.search("products", params)
```

## Several nodes

```python
client = TorqueHttpClient(
    ["http://node1:8108", "http://node2:8108"],
    api_key="your-key",
    num_retries=5,
)
```

## Collections and documents

```python
schema = {
    "name": "products",
    "fields": [
        {"name": "title", "type": "string"},
        {"name": "price", "type": "float", "sort": True},
        {"name": "brand", "type": "string", "facet": True},
    ],
}
client.create_collection(schema)

client.create_document("products", {
    "id": "1", "title": "Laptop Pro", "price": 1299.99, "brand": "Acme",
})
```

## TCP ingest

For large loads, stream over the TCP ingest protocol:

```python
from torque_http import TorqueIngestClient, DocumentEncoder

schema = {"fields": [
    {"name": "title", "type": "string"},
    {"name": "price", "type": "float"},
]}
encoder = DocumentEncoder(schema)

with TorqueIngestClient() as tcp:
    tcp.connect("localhost", 8109)
    tcp.start_ingest("products")
    tcp.send_batch(documents, encoder)
    tcp.commit()
```

## Binary import file

Write the documents to a compressed file, then upload it in one request:

```python
from torque_http import TorqueBinaryFileWriter, DocumentEncoder

encoder = DocumentEncoder(schema)
with TorqueBinaryFileWriter("data.tqbf", encoder) as writer:
    for doc in source:
        writer.write(doc)

client.import_binary("products", "data.tqbf")
```

## Documentation

- [Torque documentation](https://truespar.com/torque/docs)

## License

MIT OR Apache-2.0, at your option. See [LICENSE](LICENSE). Copyright 2025-2026 The Intelligence Company AB (publ).
