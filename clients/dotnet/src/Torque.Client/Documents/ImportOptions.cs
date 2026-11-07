namespace Torque.Http.Documents;

/// <summary>
/// Options for controlling document import behavior.
/// </summary>
public sealed class ImportOptions
{
    /// <summary>
    /// Number of documents processed per batch on the server side (default: server default).
    /// Larger values use more memory but may improve throughput on large imports.
    /// </summary>
    public int? BatchSize { get; set; }

    /// <summary>
    /// When true, each per-document result in the response includes the full document body.
    /// Increases response size significantly for large imports.
    /// </summary>
    public bool? ReturnDoc { get; set; }

    /// <summary>
    /// When true, each per-document result in the response includes the document ID.
    /// </summary>
    public bool? ReturnId { get; set; }

    /// <summary>
    /// How to handle fields with type mismatches: coerce_or_reject, coerce_or_drop, drop, reject.
    /// </summary>
    public string? DirtyValues { get; set; }
}
