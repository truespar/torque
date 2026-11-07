namespace Torque.Http.Models;

/// <summary>
/// A document ready for binary TCP ingest. Fields are ordered by schema.
/// </summary>
public sealed class TorqueDocument
{
    private string _id = "";
    private int _idUtf8ByteLength;

    /// <summary>Unique document identifier.</summary>
    public required string Id
    {
        get => _id;
        init
        {
            _id = value;
            _idUtf8ByteLength = System.Text.Encoding.UTF8.GetByteCount(value);
        }
    }

    /// <summary>Document language for stemming and tokenization.</summary>
    public Language Language { get; init; } = Language.English;
    internal FieldValue?[] Fields { get; init; } = [];

    internal int IdUtf8ByteLength => _idUtf8ByteLength;
}

/// <summary>
/// Builder for constructing TorqueDocument instances with named fields.
/// Resolves to schema-ordered field array at Build() time.
/// </summary>
public sealed class TorqueDocumentBuilder
{
    private readonly Dictionary<string, FieldValue> _fields = new();
    private string _id = "";
    private Language _language = Language.English;

    /// <summary>Sets the document ID.</summary>
    /// <param name="id">Unique document identifier.</param>
    /// <returns>This builder for chaining.</returns>
    public TorqueDocumentBuilder SetId(string id) { _id = id; return this; }

    /// <summary>Sets the document language for stemming and tokenization.</summary>
    /// <param name="language">The document language.</param>
    /// <returns>This builder for chaining.</returns>
    public TorqueDocumentBuilder SetLanguage(Language language) { _language = language; return this; }

    /// <summary>Sets a string field value.</summary>
    /// <param name="fieldName">Field name matching the collection schema.</param>
    /// <param name="value">The string value.</param>
    /// <returns>This builder for chaining.</returns>
    public TorqueDocumentBuilder Set(string fieldName, string value) { _fields[fieldName] = FieldValue.FromString(value); return this; }

    /// <summary>Sets an int32 field value.</summary>
    /// <param name="fieldName">Field name matching the collection schema.</param>
    /// <param name="value">The int32 value.</param>
    /// <returns>This builder for chaining.</returns>
    public TorqueDocumentBuilder Set(string fieldName, int value) { _fields[fieldName] = FieldValue.FromInt32(value); return this; }

    /// <summary>Sets an int64 field value.</summary>
    /// <param name="fieldName">Field name matching the collection schema.</param>
    /// <param name="value">The int64 value.</param>
    /// <returns>This builder for chaining.</returns>
    public TorqueDocumentBuilder Set(string fieldName, long value) { _fields[fieldName] = FieldValue.FromInt64(value); return this; }

    /// <summary>Sets a float64 field value.</summary>
    /// <param name="fieldName">Field name matching the collection schema.</param>
    /// <param name="value">The float64 value.</param>
    /// <returns>This builder for chaining.</returns>
    public TorqueDocumentBuilder Set(string fieldName, double value) { _fields[fieldName] = FieldValue.FromFloat64(value); return this; }

    /// <summary>Sets a boolean field value.</summary>
    /// <param name="fieldName">Field name matching the collection schema.</param>
    /// <param name="value">The boolean value.</param>
    /// <returns>This builder for chaining.</returns>
    public TorqueDocumentBuilder Set(string fieldName, bool value) { _fields[fieldName] = FieldValue.FromBool(value); return this; }

    /// <summary>Sets a geopoint field value as latitude/longitude.</summary>
    /// <param name="fieldName">Field name matching the collection schema.</param>
    /// <param name="lat">Latitude in degrees.</param>
    /// <param name="lng">Longitude in degrees.</param>
    /// <returns>This builder for chaining.</returns>
    public TorqueDocumentBuilder SetGeoPoint(string fieldName, double lat, double lng) { _fields[fieldName] = FieldValue.FromGeoPoint(lat, lng); return this; }

    /// <summary>Sets a string array field value.</summary>
    /// <param name="fieldName">Field name matching the collection schema.</param>
    /// <param name="values">The string array (null elements represent missing values).</param>
    /// <returns>This builder for chaining.</returns>
    public TorqueDocumentBuilder Set(string fieldName, string?[] values) { _fields[fieldName] = FieldValue.FromStringArray(values); return this; }

    /// <summary>Sets an int32 array field value.</summary>
    /// <param name="fieldName">Field name matching the collection schema.</param>
    /// <param name="values">The int32 array.</param>
    /// <returns>This builder for chaining.</returns>
    public TorqueDocumentBuilder Set(string fieldName, int[] values) { _fields[fieldName] = FieldValue.FromInt32Array(values.Select(v => (int?)v).ToArray()); return this; }

    /// <summary>Sets a nullable int32 array field value.</summary>
    /// <param name="fieldName">Field name matching the collection schema.</param>
    /// <param name="values">The nullable int32 array.</param>
    /// <returns>This builder for chaining.</returns>
    public TorqueDocumentBuilder Set(string fieldName, int?[] values) { _fields[fieldName] = FieldValue.FromInt32Array(values); return this; }

    /// <summary>Sets an int64 array field value.</summary>
    /// <param name="fieldName">Field name matching the collection schema.</param>
    /// <param name="values">The int64 array.</param>
    /// <returns>This builder for chaining.</returns>
    public TorqueDocumentBuilder Set(string fieldName, long[] values) { _fields[fieldName] = FieldValue.FromInt64Array(values.Select(v => (long?)v).ToArray()); return this; }

    /// <summary>Sets a nullable int64 array field value.</summary>
    /// <param name="fieldName">Field name matching the collection schema.</param>
    /// <param name="values">The nullable int64 array.</param>
    /// <returns>This builder for chaining.</returns>
    public TorqueDocumentBuilder Set(string fieldName, long?[] values) { _fields[fieldName] = FieldValue.FromInt64Array(values); return this; }

    /// <summary>Sets a float array field value (used for vector embeddings).</summary>
    /// <param name="fieldName">Field name matching the collection schema.</param>
    /// <param name="values">The float array.</param>
    /// <returns>This builder for chaining.</returns>
    public TorqueDocumentBuilder Set(string fieldName, float[] values) { _fields[fieldName] = FieldValue.FromFloatArray(values); return this; }

    /// <summary>Sets a field to null (marks an optional field as explicitly absent).</summary>
    /// <param name="fieldName">Field name matching the collection schema.</param>
    /// <returns>This builder for chaining.</returns>
    public TorqueDocumentBuilder SetNull(string fieldName) { _fields[fieldName] = FieldValue.Null; return this; }

    /// <summary>
    /// Build the document using the schema to determine field order.
    /// Fields not set are treated as null.
    /// </summary>
    /// <param name="schema">The collection schema used to order fields for binary serialization.</param>
    /// <returns>A finalized <see cref="TorqueDocument"/> ready for TCP ingest.</returns>
    /// <exception cref="InvalidOperationException">Thrown if the document ID is not set or a required field is missing.</exception>
    public TorqueDocument Build(CollectionSchema schema)
    {
        if (string.IsNullOrEmpty(_id))
            throw new InvalidOperationException("Document ID must be set before calling Build().");

        var fields = new FieldValue?[schema.Fields.Count];
        for (int i = 0; i < schema.Fields.Count; i++)
        {
            var fieldDef = schema.Fields[i];
            var hasValue = _fields.TryGetValue(fieldDef.Name, out var value);
            fields[i] = hasValue ? value : null;

            if (!fieldDef.Optional && (!hasValue || value is null || value.IsNull))
                throw new InvalidOperationException($"Required field '{fieldDef.Name}' is not set.");
        }

        return new TorqueDocument
        {
            Id = _id,
            Language = _language,
            Fields = fields,
        };
    }
}
