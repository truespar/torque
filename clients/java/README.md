# torque-http

The Java client for [Torque](https://truespar.com/torque/docs), an in-memory search engine with a Typesense-compatible API.

- Java's own `HttpClient`, with Jackson 3.1 for JSON
- **HTTP API**: search, collections, documents, keys, aliases
- **TCP ingest** (`TorqueIngestClient`) for streaming documents
- **Binary import files** (`TorqueBinaryWriter`) for bulk loads
- Several nodes, with failover and retries
- Typed `SearchParameters`, `SearchResult` and `Hit`
- Java 21+

## Installation

Add to your `pom.xml`:

```xml
<dependency>
    <groupId>com.truespar</groupId>
    <artifactId>torque-http</artifactId>
    <version>0.7.0</version>
</dependency>
```

Or Gradle:

```kotlin
implementation("com.truespar:torque-http:0.7.0")
```

## Quick start

```java
import com.truespar.torque.http.*;

try (var client = new TorqueHttpClient(
        "http://localhost:8108",
        new TorqueHttpClientOptions("your-key"))) {

    var result = client.search("products", Map.of(
        "q", "laptop",
        "query_by", "title"
    ));

    for (var hit : result.hits()) {
        System.out.println(hit.document().get("title"));
    }
}
```

## Typed search parameters

```java
SearchResult result = client.searchTyped("products",
    new SearchParameters()
        .q("laptop")
        .queryBy("title,description")
        .filterBy("price:>100")
        .perPage(20));
```

## Several nodes

```java
var options = new TorqueHttpClientOptions(
    "your-key",
    List.of(
        new Node("https", "node1.example.com", 8108),
        new Node("https", "node2.example.com", 8108)
    ),
    3,                            // numRetries
    Duration.ofMillis(100),       // retryInterval
    Duration.ofSeconds(60),       // healthcheckInterval
    Duration.ofSeconds(30),       // timeout
    Duration.ofSeconds(300),      // transferTimeout
    false,                        // allowUntrustedCertificate
    false                         // sendApiKeyAsQueryParam
);
var client = new TorqueHttpClient(options);
```

## Collections and documents

```java
var schema = Map.<String, Object>of(
    "name", "products",
    "fields", List.of(
        Map.of("name", "title", "type", "string"),
        Map.of("name", "price", "type", "float", "sort", true),
        Map.of("name", "brand", "type", "string", "facet", true)
    )
);
client.createCollection(schema);

client.createDocument("products", Map.of(
    "id", "1", "title", "Laptop Pro", "price", 1299.99, "brand", "Acme"
));
```

## TCP ingest

```java
var schema = List.<Map<String,Object>>of(
    Map.of("name", "title", "type", "string"),
    Map.of("name", "price", "type", "float")
);

var tcp = new TorqueIngestClient();
tcp.connect("localhost", 8109);
tcp.startIngest("products");
tcp.sendBatch(documents, schema);
tcp.commit();
tcp.close();
```

## Binary import file

```java
var writer = new TorqueBinaryWriter(schema, Path.of("data.tqbf"));
for (var doc : source) {
    writer.write(doc);
}
writer.finalizeTqbf();

client.importBinary("products", Path.of("data.tqbf"));
```

## Building from source

```bash
mvn clean package
```

To publish to Maven Central:

```bash
mvn clean deploy -P release
```

## Documentation

- [Torque documentation](https://truespar.com/torque/docs)

## License

MIT OR Apache-2.0, at your option. See [LICENSE](LICENSE). Copyright 2025-2026 The Intelligence Company AB (publ).
