# @truespar/torque-http

The Node.js client for [Torque](https://truespar.com/torque/docs), an in-memory search engine with a Typesense-compatible API.

- No dependencies: Node's own `fetch`, `net` and `zlib`
- ESM only, TypeScript declarations included
- **HTTP API**: search, collections, documents, keys, aliases
- **TCP ingest** (`TorqueIngestClient`) for streaming documents
- **Binary import files** (`TorqueBinaryWriter`) for bulk loads
- Several nodes, with failover and retries
- Node.js 20+

## Installation

```bash
npm install @truespar/torque-http
```

## Quick start

```js
import { TorqueHttpClient } from '@truespar/torque-http';

const client = new TorqueHttpClient('http://localhost:8108', {
  apiKey: 'your-key',
});

// Search
const results = await client.search('products', {
  q: 'laptop',
  query_by: 'title',
});

for (const hit of results.hits) {
  console.log(hit.document.title);
}
```

## Several nodes

```js
const client = new TorqueHttpClient({
  apiKey: 'your-key',
  nodes: [
    { host: 'node1.example.com', port: 8108, protocol: 'https' },
    { host: 'node2.example.com', port: 8108, protocol: 'https' },
  ],
  retries: 5,
});
```

## Collections and documents

```js
await client.createCollection({
  name: 'products',
  fields: [
    { name: 'title', type: 'string' },
    { name: 'price', type: 'float', sort: true },
    { name: 'brand', type: 'string', facet: true },
  ],
});

await client.createDocument('products', {
  id: '1', title: 'Laptop Pro', price: 1299.99, brand: 'Acme',
});
```

## TCP ingest

For large loads, stream over the TCP ingest protocol:

```js
import { TorqueIngestClient } from '@truespar/torque-http';

const schema = [
  { name: 'title', type: 'string' },
  { name: 'price', type: 'float' },
];

const tcp = new TorqueIngestClient();
await tcp.connect('localhost', 8109);
await tcp.startIngest('products');
await tcp.sendBatch(documents, schema);
await tcp.commit();
tcp.close();
```

## Binary import file

Write the documents to a compressed file, then upload it in one request:

```js
import { TorqueBinaryWriter } from '@truespar/torque-http';

const writer = new TorqueBinaryWriter(schema, 'data.tqbf');
for (const doc of source) writer.write(doc);
writer.finalize();

await client.importBinary('products', 'data.tqbf');
```

## Documentation

- [Torque documentation](https://truespar.com/torque/docs)

## License

MIT OR Apache-2.0, at your option. See [LICENSE](LICENSE). Copyright 2025-2026 The Intelligence Company AB (publ).
