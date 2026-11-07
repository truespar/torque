# Torque.Http.Newtonsoft

`TqbfNewtonsoftEncoder` for the [Torque](https://truespar.com/torque/docs) .NET client: adds application objects to a `TqbfSegmentBuilder` run as the records their Newtonsoft JSON serialization would give, without writing JSON.

```csharp
using Torque.Http.Tqbf;

using var builder = new TqbfSegmentBuilder(schemaJson);
var encoder = new TqbfNewtonsoftEncoder(jsonSettings);
using var run = builder.BeginRun(0);
foreach (var document in documents)
    encoder.Add(run, document);
run.End();
builder.Finish("companies.tqbf");
```

Depends on `Torque.Http` and `Newtonsoft.Json`. .NET 10+.

## License

MIT OR Apache-2.0, at your option. Copyright 2025-2026 The Intelligence Company AB (publ).
