using System.Text.Json.Serialization;

namespace Torque.Http.Models;

/// <summary>
/// Field type identifiers matching Rust FieldType::as_str().
/// </summary>
public static class TorqueFieldType
{
    /// <summary>Single string value.</summary>
    public const string String = "string";
    /// <summary>32-bit signed integer.</summary>
    public const string Int32 = "int32";
    /// <summary>64-bit signed integer.</summary>
    public const string Int64 = "int64";
    /// <summary>64-bit floating point number.</summary>
    public const string Float = "float";
    /// <summary>Boolean value.</summary>
    public const string Bool = "bool";
    /// <summary>Array of strings.</summary>
    public const string StringArray = "string[]";
    /// <summary>Array of 32-bit signed integers.</summary>
    public const string Int32Array = "int32[]";
    /// <summary>Array of 64-bit signed integers.</summary>
    public const string Int64Array = "int64[]";
    /// <summary>Array of floats (also used for vector embeddings).</summary>
    public const string FloatArray = "float[]";
    /// <summary>Geographic point as latitude/longitude pair.</summary>
    public const string GeoPoint = "geopoint";
}

/// <summary>
/// Index mode identifiers matching Rust IndexMode variants.
/// </summary>
public static class TorqueIndexMode
{
    /// <summary>Batch mode (default). Documents are indexed via the ingest pipeline with explicit commit.</summary>
    public const string Batch = "batch";

    /// <summary>Realtime mode. Documents are searchable immediately after upsert without a commit step.</summary>
    public const string Realtime = "realtime";
}

/// <summary>
/// Vector distance metric.
/// </summary>
[JsonConverter(typeof(JsonStringEnumConverter<VecDist>))]
public enum VecDist
{
    /// <summary>Cosine similarity distance.</summary>
    [JsonStringEnumMemberName("cosine")]
    Cosine,

    /// <summary>Inner product (dot product) distance.</summary>
    [JsonStringEnumMemberName("ip")]
    DotProduct,

    /// <summary>Euclidean (L2) distance.</summary>
    [JsonStringEnumMemberName("l2")]
    Euclidean,
}

/// <summary>
/// Field definition matching Rust FieldDef.
/// </summary>
public sealed class FieldDefinition
{
    /// <summary>Field name.</summary>
    [JsonPropertyName("name")]
    public required string Name { get; set; }

    /// <summary>Field type (use <see cref="TorqueFieldType"/> constants, e.g. <c>"string"</c>, <c>"int32"</c>).</summary>
    [JsonPropertyName("type")]
    public required string Type { get; set; }

    /// <summary>Whether this field is available for faceting.</summary>
    [JsonPropertyName("facet")]
    [JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingDefault)]
    public bool Facet { get; set; }

    /// <summary>Whether this field is indexed for search (default true).</summary>
    [JsonPropertyName("index")]
    [JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingDefault)]
    public bool Index { get; set; } = true;

    /// <summary>Whether this field can be omitted from documents.</summary>
    [JsonPropertyName("optional")]
    [JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingDefault)]
    public bool Optional { get; set; }

    /// <summary>Whether this field is available for sorting.</summary>
    [JsonPropertyName("sort")]
    [JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingDefault)]
    public bool Sort { get; set; }

    /// <summary>Whether stemming is applied to this field (default true).</summary>
    [JsonPropertyName("stem")]
    [JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingDefault)]
    public bool Stem { get; set; } = true;

    /// <summary>Language locale for stemming and tokenization (e.g. <c>"en"</c>, <c>"sv"</c>).</summary>
    [JsonPropertyName("locale")]
    [JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)]
    public string? Locale { get; set; }

    /// <summary>Number of dimensions for vector fields.</summary>
    [JsonPropertyName("num_dim")]
    [JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)]
    public int? NumDim { get; set; }

    /// <summary>Distance metric for vector fields.</summary>
    [JsonPropertyName("vec_dist")]
    [JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)]
    public VecDist? VecDist { get; set; }

    /// <summary>Set to true in a schema update to remove this field from the collection.</summary>
    [JsonPropertyName("drop")]
    [JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingDefault)]
    public bool Drop { get; set; }

    /// <summary>Reference to another collection for JOINs (reserved for v2).</summary>
    [JsonPropertyName("reference")]
    [JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)]
    public string? Reference { get; set; }
}

/// <summary>
/// Collection schema for creation requests.
/// </summary>
public sealed class CollectionSchema
{
    /// <summary>Collection name.</summary>
    [JsonPropertyName("name")]
    public required string Name { get; set; }

    /// <summary>Field definitions for the collection.</summary>
    [JsonPropertyName("fields")]
    public required List<FieldDefinition> Fields { get; set; }

    /// <summary>Field used for default sorting when no sort_by is specified.</summary>
    [JsonPropertyName("default_sorting_field")]
    [JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)]
    public string? DefaultSortingField { get; set; }

    /// <summary>Whether nested object fields are enabled for this collection.</summary>
    [JsonPropertyName("enable_nested_fields")]
    [JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingDefault)]
    public bool EnableNestedFields { get; set; }

    /// <summary>Special characters to index as part of tokens (e.g. <c>'+'</c>, <c>'#'</c>).</summary>
    [JsonPropertyName("symbols_to_index")]
    [JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingDefault)]
    public List<char>? SymbolsToIndex { get; set; }

    /// <summary>Characters that split tokens during indexing (e.g. <c>'-'</c>, <c>'/'</c>).</summary>
    [JsonPropertyName("token_separators")]
    [JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingDefault)]
    public List<char>? TokenSeparators { get; set; }

    /// <summary>Default language for stemming and tokenization across all fields.</summary>
    [JsonPropertyName("default_language")]
    [JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)]
    public string? DefaultLanguage { get; set; }

    /// <summary>Index mode: <c>"batch"</c> (default) or <c>"realtime"</c>. Use <see cref="TorqueIndexMode"/> constants.</summary>
    [JsonPropertyName("index_mode")]
    [JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)]
    public string? IndexMode { get; set; }
}

/// <summary>
/// Collection response from the server (includes runtime fields).
/// </summary>
public sealed class CollectionResponse
{
    /// <summary>Collection name.</summary>
    [JsonPropertyName("name")]
    public string Name { get; set; } = "";

    /// <summary>Number of documents currently in the collection.</summary>
    [JsonPropertyName("num_documents")]
    public uint NumDocuments { get; set; }

    /// <summary>Field definitions for the collection.</summary>
    [JsonPropertyName("fields")]
    public List<FieldDefinition> Fields { get; set; } = [];

    /// <summary>Default sorting field, if configured.</summary>
    [JsonPropertyName("default_sorting_field")]
    [JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)]
    public string? DefaultSortingField { get; set; }

    /// <summary>Unix timestamp (seconds) when the collection was created.</summary>
    [JsonPropertyName("created_at")]
    public ulong CreatedAt { get; set; }
}
