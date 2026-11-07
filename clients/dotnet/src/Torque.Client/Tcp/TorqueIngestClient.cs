using System.Buffers;
using System.Buffers.Binary;
using System.Net.Sockets;
using Torque.Http.Exceptions;
using Torque.Http.Models;

namespace Torque.Http.Tcp;

/// <summary>
/// TCP ingest client for streaming documents to Torque via the binary protocol.
/// Thread-safe via SemaphoreSlim. Implements IAsyncDisposable for cleanup.
///
/// Uses ArrayPool-rented buffers for zero-allocation I/O:
/// - _sendBuf: growable send buffer (64KB initial, grows as needed)
/// - _recvBuf: fixed receive buffer (4KB), holds server responses
/// All writes use a single WriteAsync call per frame.
/// </summary>
public sealed class TorqueIngestClient : IAsyncDisposable
{
    private const int InitialSendBufSize = 64 * 1024;
    private const int RecvBufSize = 4 * 1024;

    private readonly TorqueIngestOptions _options;
    private readonly SemaphoreSlim _semaphore = new(1, 1);
    private readonly IngestSession _session = new();
    private TcpClient? _tcpClient;
    private NetworkStream? _stream;
    private byte[] _sendBuf;
    private byte[] _recvBuf;
    private long _totalBytesSent;
    private long _totalBytesReceived;
    private long _totalDocsSent;

    /// <summary>
    /// Creates a new TCP ingest client with the specified options.
    /// </summary>
    /// <param name="options">Ingest client options, or null for defaults.</param>
    public TorqueIngestClient(TorqueIngestOptions? options = null)
    {
        _options = options ?? new TorqueIngestOptions();
        _sendBuf = ArrayPool<byte>.Shared.Rent(InitialSendBufSize);
        _recvBuf = ArrayPool<byte>.Shared.Rent(RecvBufSize);
    }

    /// <summary>
    /// Ensure _sendBuf has at least totalNeeded bytes of capacity.
    /// Grows via ArrayPool re-rent with existing content preserved.
    /// Grows via ArrayPool re-rent when needed.
    /// </summary>
    private void EnsureSendCapacity(int totalNeeded)
    {
        if (totalNeeded <= _sendBuf.Length) return;

        int newSize = Math.Max(_sendBuf.Length * 2, totalNeeded);
        byte[] newBuf = ArrayPool<byte>.Shared.Rent(newSize);
        Buffer.BlockCopy(_sendBuf, 0, newBuf, 0, _sendBuf.Length);
        ArrayPool<byte>.Shared.Return(_sendBuf);
        _sendBuf = newBuf;
    }

    private async Task AcquireLockAsync(CancellationToken ct)
    {
        if (!await _semaphore.WaitAsync(_options.SemaphoreTimeoutMs, ct).ConfigureAwait(false))
            throw new TimeoutException(
                $"Failed to acquire ingest client lock within {_options.SemaphoreTimeoutMs}ms. " +
                "Another operation may be in progress.");
    }

    private async Task<int> ReadFrameWithTimeoutAsync(CancellationToken ct)
    {
        using var linkedCts = CancellationTokenSource.CreateLinkedTokenSource(ct);
        linkedCts.CancelAfter(_options.ReadTimeoutMs);
        try
        {
            return await TcpFrameCodec.ReadFrameAsync(_stream!, _recvBuf, linkedCts.Token).ConfigureAwait(false);
        }
        catch (OperationCanceledException) when (!ct.IsCancellationRequested)
        {
            throw new TorqueConnectionException(
                $"Read timed out after {_options.ReadTimeoutMs}ms waiting for server response.");
        }
    }

    /// <summary>
    /// Connect to the Torque TCP ingest server with retry on transient failures.
    /// </summary>
    public async Task ConnectAsync(CancellationToken ct = default)
    {
        await AcquireLockAsync(ct).ConfigureAwait(false);
        try
        {
            if (_tcpClient is not null)
                throw new InvalidOperationException("Already connected");

            for (int attempt = 0; ; attempt++)
            {
                try
                {
                    _tcpClient = new TcpClient { NoDelay = true };
                    await _tcpClient.ConnectAsync(_options.Host, _options.Port, ct).ConfigureAwait(false);
                    _stream = _tcpClient.GetStream();
                    return;
                }
                catch (SocketException) when (attempt < _options.MaxRetries)
                {
                    _tcpClient?.Dispose();
                    _tcpClient = null;
                    await Task.Delay(ComputeBackoff(attempt + 1), ct).ConfigureAwait(false);
                }
                catch (SocketException ex)
                {
                    _tcpClient?.Dispose();
                    _tcpClient = null;
                    throw new TorqueConnectionException(
                        $"Failed to connect to {_options.Host}:{_options.Port} after {_options.MaxRetries + 1} attempts", ex);
                }
            }
        }
        finally
        {
            _semaphore.Release();
        }
    }

    /// <summary>
    /// Start an ingest session for the given collection with retry on transient failures.
    /// </summary>
    public async Task<BatchAck> StartIngestAsync(string collection, CancellationToken ct = default)
    {
        await AcquireLockAsync(ct).ConfigureAwait(false);
        try
        {
            EnsureConnected();

            for (int attempt = 0; ; attempt++)
            {
                try
                {
                    _session.TransitionToStreaming();

                    int msgSize = MessageEncoder.ComputeStartIngestSize(collection);
                    EnsureSendCapacity(4 + msgSize);

                    int pos = 4;
                    MessageEncoder.EncodeStartIngest(_sendBuf, ref pos, collection);

                    await TcpFrameCodec.WriteFrameAsync(_stream!, _sendBuf, pos, ct).ConfigureAwait(false);

                    int responseLen = await ReadFrameWithTimeoutAsync(ct).ConfigureAwait(false);
                    var result = MessageDecoder.Decode(_recvBuf.AsSpan(0, responseLen));
                    return (BatchAck)result;
                }
                catch (Exception ex) when (IsTransientTcpException(ex) && attempt < _options.MaxRetries)
                {
                    _session.TransitionToIdle();
                    await ReconnectAsync(ct).ConfigureAwait(false);
                }
                catch (TorqueProtocolException)
                {
                    _session.TransitionToIdle();
                    throw;
                }
                catch (Exception ex) when (IsTransientTcpException(ex))
                {
                    _session.TransitionToIdle();
                    throw new TorqueConnectionException(
                        $"StartIngest failed after {_options.MaxRetries + 1} attempts", ex);
                }
            }
        }
        finally
        {
            _semaphore.Release();
        }
    }

    /// <summary>
    /// Send all documents, automatically chunking into batches of <paramref name="batchSize"/>.
    /// Each batch is sent as a separate TCP frame. All documents are accumulated on the server
    /// until <see cref="CommitAsync"/> is called.
    /// </summary>
    /// <param name="documents">All documents to send.</param>
    /// <param name="schema">Collection schema for binary encoding.</param>
    /// <param name="batchSize">Documents per TCP frame. Default: 50,000.</param>
    /// <param name="ct">Cancellation token.</param>
    /// <returns>The last <see cref="BatchAck"/> with cumulative document count.</returns>
    public async Task<BatchAck> SendDocumentsAsync(
        IReadOnlyList<TorqueDocument> documents,
        CollectionSchema schema,
        int batchSize = 50_000,
        CancellationToken ct = default)
    {
        if (documents.Count == 0)
            throw new ArgumentException("No documents to send.", nameof(documents));

        var array = documents as TorqueDocument[] ?? documents.ToArray();
        BatchAck lastAck = null!;
        for (int offset = 0; offset < array.Length; offset += batchSize)
        {
            int count = Math.Min(batchSize, array.Length - offset);
            var chunk = new ArraySegment<TorqueDocument>(array, offset, count);
            lastAck = await SendBatchAsync(chunk, schema, ct).ConfigureAwait(false);
        }
        return lastAck;
    }

    /// <summary>
    /// Send a single batch of documents as one TCP frame. Returns server acknowledgement
    /// with cumulative count. For automatic chunking, use <see cref="SendDocumentsAsync"/> instead.
    /// Does NOT retry on failure — data integrity risk. Wraps transient errors
    /// in TorqueConnectionException and resets session to Idle.
    /// </summary>
    public async Task<BatchAck> SendBatchAsync(IReadOnlyList<TorqueDocument> documents, CollectionSchema schema, CancellationToken ct = default)
    {
        await AcquireLockAsync(ct).ConfigureAwait(false);
        try
        {
            EnsureConnected();
            _session.ValidateStreaming();

            // Pre-sum total batch size: single capacity check, no reallocations during encode
            int totalSize = 4 + 5; // frame header + msg type + doc count
            for (int i = 0; i < documents.Count; i++)
                totalSize += DocumentEncoder.ComputeSize(documents[i], schema);
            EnsureSendCapacity(totalSize);

            int pos = 4;
            MessageEncoder.EncodeDocBatchHeader(_sendBuf, ref pos, (uint)documents.Count);

            for (int i = 0; i < documents.Count; i++)
                DocumentEncoder.Encode(documents[i], schema, _sendBuf, ref pos);

            try
            {
                var sw = System.Diagnostics.Stopwatch.StartNew();
                await TcpFrameCodec.WriteFrameAsync(_stream!, _sendBuf, pos, ct).ConfigureAwait(false);
                int responseLen = await ReadFrameWithTimeoutAsync(ct).ConfigureAwait(false);
                sw.Stop();

                _totalBytesSent += pos;
                _totalBytesReceived += responseLen + 4; // payload + frame header
                _totalDocsSent += documents.Count;

                if (_options.OnBatchStats is { } callback)
                {
                    callback(new IngestStats
                    {
                        DocsInBatch = documents.Count,
                        BytesSent = pos,
                        BytesReceived = responseLen + 4,
                        Elapsed = sw.Elapsed,
                        TotalBytesSent = _totalBytesSent,
                        TotalBytesReceived = _totalBytesReceived,
                        TotalDocsSent = _totalDocsSent,
                    });
                }

                var result = MessageDecoder.Decode(_recvBuf.AsSpan(0, responseLen));
                return (BatchAck)result;
            }
            catch (Exception ex) when (IsTransientTcpException(ex))
            {
                _session.TransitionToIdle();
                throw new TorqueConnectionException("SendBatch failed due to connection error. Session reset to Idle.", ex);
            }
        }
        finally
        {
            _semaphore.Release();
        }
    }

    /// <summary>
    /// Commit the current ingest session. Returns immediately with a BatchAck —
    /// the index rebuild runs asynchronously on the server. Poll
    /// <c>GET /collections/{name}</c> and check <c>num_documents</c> to confirm
    /// the rebuild has completed.
    /// </summary>
    public async Task<BatchAck> CommitAsync(CancellationToken ct = default)
    {
        await AcquireLockAsync(ct).ConfigureAwait(false);
        try
        {
            EnsureConnected();
            _session.ValidateStreaming();

            int pos = 4;
            MessageEncoder.EncodeCommit(_sendBuf, ref pos);

            try
            {
                await TcpFrameCodec.WriteFrameAsync(_stream!, _sendBuf, pos, ct).ConfigureAwait(false);
                int responseLen = await ReadFrameWithTimeoutAsync(ct).ConfigureAwait(false);
                var result = MessageDecoder.Decode(_recvBuf.AsSpan(0, responseLen));
                _session.TransitionToIdle();
                return (BatchAck)result;
            }
            catch (Exception ex) when (IsTransientTcpException(ex))
            {
                _session.TransitionToIdle();
                throw new TorqueConnectionException("Commit failed due to connection error. Session reset to Idle.", ex);
            }
        }
        catch (Exception) when (_session.CurrentState == IngestSession.State.Streaming)
        {
            _session.TransitionToIdle();
            throw;
        }
        finally
        {
            _semaphore.Release();
        }
    }

    /// <summary>
    /// Abort the current ingest session, discarding all documents.
    /// Does NOT retry — wraps transient errors and resets session.
    /// </summary>
    public async Task<BatchAck> AbortAsync(CancellationToken ct = default)
    {
        await AcquireLockAsync(ct).ConfigureAwait(false);
        try
        {
            EnsureConnected();
            _session.ValidateStreaming();

            int pos = 4;
            MessageEncoder.EncodeAbort(_sendBuf, ref pos);

            try
            {
                await TcpFrameCodec.WriteFrameAsync(_stream!, _sendBuf, pos, ct).ConfigureAwait(false);
                int responseLen = await ReadFrameWithTimeoutAsync(ct).ConfigureAwait(false);
                var result = MessageDecoder.Decode(_recvBuf.AsSpan(0, responseLen));
                _session.TransitionToIdle();
                return (BatchAck)result;
            }
            catch (Exception ex) when (IsTransientTcpException(ex))
            {
                _session.TransitionToIdle();
                throw new TorqueConnectionException("Abort failed due to connection error. Session reset to Idle.", ex);
            }
        }
        catch (Exception) when (_session.CurrentState == IngestSession.State.Streaming)
        {
            _session.TransitionToIdle();
            throw;
        }
        finally
        {
            _semaphore.Release();
        }
    }

    /// <summary>
    /// Upsert a single document to a realtime collection. Stateless — no StartIngest/Commit flow needed.
    /// Automatically retries with exponential backoff on server backpressure.
    /// </summary>
    public Task<BatchAck> UpsertAsync(
        string collection, TorqueDocument document, CollectionSchema schema, CancellationToken ct = default) =>
        UpsertBatchAsync(collection, [document], schema, ct);

    /// <summary>
    /// Upsert a batch of documents to a realtime collection. Stateless — no StartIngest/Commit flow needed.
    /// Automatically retries with exponential backoff on server backpressure.
    /// </summary>
    public async Task<BatchAck> UpsertBatchAsync(
        string collection, IReadOnlyList<TorqueDocument> documents, CollectionSchema schema, CancellationToken ct = default)
    {
        if (documents.Count == 0)
            throw new ArgumentException("No documents to upsert.", nameof(documents));

        await AcquireLockAsync(ct).ConfigureAwait(false);
        try
        {
            EnsureConnected();

            int headerSize = 4 + MessageEncoder.ComputeUpsertHeaderSize(collection);
            int totalSize = headerSize;
            for (int i = 0; i < documents.Count; i++)
                totalSize += DocumentEncoder.ComputeSize(documents[i], schema);
            EnsureSendCapacity(totalSize);

            for (int attempt = 0; ; attempt++)
            {
                int pos = 4;
                MessageEncoder.EncodeUpsertHeader(_sendBuf, ref pos, collection,
                    ProtocolConstants.ActionUpsert, (uint)documents.Count);
                for (int i = 0; i < documents.Count; i++)
                    DocumentEncoder.Encode(documents[i], schema, _sendBuf, ref pos);

                await TcpFrameCodec.WriteFrameAsync(_stream!, _sendBuf, pos, ct).ConfigureAwait(false);
                int responseLen = await ReadFrameWithTimeoutAsync(ct).ConfigureAwait(false);
                var result = MessageDecoder.Decode(_recvBuf.AsSpan(0, responseLen));

                if (result is BackpressureResult bp)
                {
                    _options.OnBackpressure?.Invoke(bp);
                    if (attempt >= _options.MaxBackpressureRetries)
                        throw new TorqueConnectionException(
                            $"Server backpressure after {attempt + 1} retries. The collection is flushing its realtime writes.");
                    int delay = Math.Min(_options.InitialBackoffMs * (1 << attempt), _options.MaxBackoffMs);
                    await Task.Delay(delay, ct).ConfigureAwait(false);
                    continue;
                }

                return (BatchAck)result;
            }
        }
        finally
        {
            _semaphore.Release();
        }
    }

    /// <summary>
    /// Delete a single document from a realtime collection by external ID.
    /// </summary>
    public Task<BatchAck> DeleteAsync(
        string collection, string documentId, CancellationToken ct = default) =>
        DeleteBatchAsync(collection, [documentId], ct);

    /// <summary>
    /// Delete multiple documents from a realtime collection by external IDs.
    /// Automatically retries with exponential backoff on server backpressure.
    /// </summary>
    public async Task<BatchAck> DeleteBatchAsync(
        string collection, IReadOnlyList<string> documentIds, CancellationToken ct = default)
    {
        if (documentIds.Count == 0)
            throw new ArgumentException("No document IDs to delete.", nameof(documentIds));

        await AcquireLockAsync(ct).ConfigureAwait(false);
        try
        {
            EnsureConnected();

            int headerSize = 4 + MessageEncoder.ComputeUpsertHeaderSize(collection);
            int idsSize = MessageEncoder.ComputeDeleteIdsSize(documentIds);
            EnsureSendCapacity(headerSize + idsSize);

            for (int attempt = 0; ; attempt++)
            {
                int pos = 4;
                MessageEncoder.EncodeUpsertHeader(_sendBuf, ref pos, collection,
                    ProtocolConstants.ActionDelete, (uint)documentIds.Count);
                MessageEncoder.EncodeDeleteIds(_sendBuf, ref pos, documentIds);

                await TcpFrameCodec.WriteFrameAsync(_stream!, _sendBuf, pos, ct).ConfigureAwait(false);
                int responseLen = await ReadFrameWithTimeoutAsync(ct).ConfigureAwait(false);
                var result = MessageDecoder.Decode(_recvBuf.AsSpan(0, responseLen));

                if (result is BackpressureResult bp)
                {
                    _options.OnBackpressure?.Invoke(bp);
                    if (attempt >= _options.MaxBackpressureRetries)
                        throw new TorqueConnectionException(
                            $"Server backpressure after {attempt + 1} retries. The collection is flushing its realtime writes.");
                    int delay = Math.Min(_options.InitialBackoffMs * (1 << attempt), _options.MaxBackoffMs);
                    await Task.Delay(delay, ct).ConfigureAwait(false);
                    continue;
                }

                return (BatchAck)result;
            }
        }
        finally
        {
            _semaphore.Release();
        }
    }

    private void EnsureConnected()
    {
        if (_stream is null)
            throw new InvalidOperationException("Not connected. Call ConnectAsync first.");
    }

    private async Task ReconnectAsync(CancellationToken ct)
    {
        _stream?.Dispose();
        _tcpClient?.Dispose();
        _stream = null;
        _tcpClient = null;

        _tcpClient = new TcpClient { NoDelay = true };
        try
        {
            await _tcpClient.ConnectAsync(_options.Host, _options.Port, ct).ConfigureAwait(false);
            _stream = _tcpClient.GetStream();
        }
        catch
        {
            _tcpClient?.Dispose();
            _tcpClient = null;
            throw;
        }
    }

    private int ComputeBackoff(int attempt)
    {
        int baseDelay = _options.InitialBackoffMs * (1 << (attempt - 1));
        int jitter = Random.Shared.Next(0, baseDelay / 2 + 1);
        return Math.Min(baseDelay + jitter, _options.MaxBackoffMs);
    }

    private static bool IsTransientTcpException(Exception ex) =>
        ex is SocketException or IOException or TorqueConnectionException;

    /// <summary>
    /// Disposes the TCP connection and returns pooled buffers.
    /// </summary>
    public async ValueTask DisposeAsync()
    {
        _stream?.Dispose();
        _tcpClient?.Dispose();
        _stream = null;
        _tcpClient = null;
        _semaphore.Dispose();

        ArrayPool<byte>.Shared.Return(_sendBuf);
        ArrayPool<byte>.Shared.Return(_recvBuf);
        _sendBuf = null!;
        _recvBuf = null!;
    }
}
