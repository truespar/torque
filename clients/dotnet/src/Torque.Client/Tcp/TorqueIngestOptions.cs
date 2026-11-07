using Torque.Http.Models;

namespace Torque.Http.Tcp;

/// <summary>
/// Configuration options for TorqueIngestClient.
/// </summary>
public sealed class TorqueIngestOptions
{
    /// <summary>TCP ingest server hostname (default: "localhost").</summary>
    public string Host { get; set; } = "localhost";

    /// <summary>TCP ingest server port (default: 8109).</summary>
    public int Port { get; set; } = 8109;

    /// <summary>Maximum number of connection retry attempts (default: 3).</summary>
    public int MaxRetries { get; set; } = 3;

    /// <summary>Initial backoff delay in milliseconds for retries (default: 200ms).</summary>
    public int InitialBackoffMs { get; set; } = 200;

    /// <summary>Maximum backoff delay in milliseconds for retries (default: 5000ms).</summary>
    public int MaxBackoffMs { get; set; } = 5000;

    /// <summary>Timeout in milliseconds for reading server responses (default: 30s).</summary>
    public int ReadTimeoutMs { get; set; } = 30_000;

    /// <summary>Timeout in milliseconds for acquiring the concurrency lock (default: 30s).</summary>
    public int SemaphoreTimeoutMs { get; set; } = 30_000;

    /// <summary>
    /// Maximum number of retries when server returns backpressure during upsert.
    /// </summary>
    public int MaxBackpressureRetries { get; set; } = 10;

    /// <summary>
    /// Called after each batch is sent and acknowledged. Reports wire-level stats
    /// including bytes sent/received, timing, and cumulative totals.
    /// </summary>
    public Action<IngestStats>? OnBatchStats { get; set; }

    /// <summary>
    /// Called when the server returns a backpressure signal during upsert.
    /// </summary>
    public Action<BackpressureResult>? OnBackpressure { get; set; }
}
