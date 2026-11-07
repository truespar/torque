namespace Torque.Http.Models;

/// <summary>
/// Acknowledgement from the server after a DocBatch or StartIngest.
/// </summary>
public sealed class BatchAck
{
    /// <summary>Monotonically increasing sequence number for this batch.</summary>
    public ulong BatchSeq { get; init; }

    /// <summary>Cumulative number of documents received by the server in this session.</summary>
    public ulong DocsReceived { get; init; }
}

/// <summary>
/// Result of a Commit operation.
/// </summary>
public sealed class CommitResult
{
    /// <summary>Whether the commit and index rebuild completed successfully.</summary>
    public bool Success { get; init; }

    /// <summary>Total number of documents indexed after the commit.</summary>
    public ulong DocsIndexed { get; init; }

    /// <summary>Time taken for the index rebuild in milliseconds.</summary>
    public ulong BuildMs { get; init; }
}

/// <summary>
/// Server response when a realtime collection's segments in memory are full and it cannot accept more upserts until a flush.
/// </summary>
public sealed class BackpressureResult
{
    /// <summary>Suggested delay in milliseconds before retrying the upsert.</summary>
    public uint RetryAfterMs { get; init; }
}

/// <summary>
/// Wire-level stats emitted after each batch send.
/// </summary>
public sealed class IngestStats
{
    /// <summary>Number of documents in this batch.</summary>
    public int DocsInBatch { get; init; }
    /// <summary>Bytes sent over the wire for this batch (including frame header).</summary>
    public int BytesSent { get; init; }
    /// <summary>Bytes received from the server for the ack.</summary>
    public int BytesReceived { get; init; }
    /// <summary>Elapsed time for this batch send + ack round-trip.</summary>
    public TimeSpan Elapsed { get; init; }
    /// <summary>Cumulative bytes sent across all batches in this session.</summary>
    public long TotalBytesSent { get; init; }
    /// <summary>Cumulative bytes received across all batches in this session.</summary>
    public long TotalBytesReceived { get; init; }
    /// <summary>Cumulative documents sent across all batches in this session.</summary>
    public long TotalDocsSent { get; init; }
}
