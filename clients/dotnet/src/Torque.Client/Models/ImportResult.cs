using System.Text.Json.Serialization;

namespace Torque.Http.Models;

/// <summary>
/// Per-line result from JSONL document import.
/// </summary>
public sealed class ImportResult
{
    /// <summary>Whether this document was imported successfully.</summary>
    [JsonPropertyName("success")]
    public bool Success { get; set; }

    /// <summary>Error message if the import failed for this document, null on success.</summary>
    [JsonPropertyName("error")]
    [JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)]
    public string? Error { get; set; }
}
