using System.Diagnostics;

namespace Torque.Http.Http;

/// <summary>
/// Lock-free round-robin node pool with health tracking and automatic failover.
/// </summary>
internal sealed class NodePool
{
    private readonly NodeState[] _nodes;
    private readonly TimeSpan _cooldown;
    private int _index;

    public NodePool(IReadOnlyList<Uri> nodes, TimeSpan cooldown)
    {
        ArgumentOutOfRangeException.ThrowIfZero(nodes.Count, nameof(nodes));
        _nodes = new NodeState[nodes.Count];
        for (int i = 0; i < nodes.Count; i++)
            _nodes[i] = new NodeState(nodes[i]);
        _cooldown = cooldown;
    }

    /// <summary>
    /// Get the next healthy node using round-robin. If all nodes are unhealthy,
    /// returns the one that has been unhealthy the longest (most likely recovered).
    /// </summary>
    public Uri GetNext()
    {
        int count = _nodes.Length;
        if (count == 1)
        {
            ref var single = ref _nodes[0];
            return single.BaseUri;
        }

        int start = Interlocked.Increment(ref _index);

        // First pass: find a healthy node
        for (int i = 0; i < count; i++)
        {
            int idx = (int)((uint)(start + i) % (uint)count);
            ref var node = ref _nodes[idx];
            if (node.IsHealthy || node.CooldownExpired(_cooldown))
                return node.BaseUri;
        }

        // All unhealthy and in cooldown — pick the oldest failure (most likely recovered)
        int bestIdx = 0;
        long oldestTicks = long.MaxValue;
        for (int i = 0; i < count; i++)
        {
            long t = Volatile.Read(ref _nodes[i].LastFailureTicks);
            if (t < oldestTicks)
            {
                oldestTicks = t;
                bestIdx = i;
            }
        }
        return _nodes[bestIdx].BaseUri;
    }

    public void ReportSuccess(Uri node)
    {
        ref var state = ref FindNode(node);
        state.IsHealthy = true;
        state.ConsecutiveFailures = 0;
    }

    public void ReportFailure(Uri node)
    {
        ref var state = ref FindNode(node);
        state.ConsecutiveFailures++;
        state.LastFailureTicks = Stopwatch.GetTimestamp();
        if (state.ConsecutiveFailures >= 3)
            state.IsHealthy = false;
    }

    private ref NodeState FindNode(Uri uri)
    {
        for (int i = 0; i < _nodes.Length; i++)
        {
            if (ReferenceEquals(_nodes[i].BaseUri, uri) || _nodes[i].BaseUri == uri)
                return ref _nodes[i];
        }
        return ref _nodes[0];
    }

    private struct NodeState(Uri baseUri)
    {
        public readonly Uri BaseUri = baseUri;
        public bool IsHealthy = true;
        public int ConsecutiveFailures = 0;
        public long LastFailureTicks = 0;

        public readonly bool CooldownExpired(TimeSpan cooldown)
        {
            if (LastFailureTicks == 0) return true;
            var elapsed = Stopwatch.GetElapsedTime(LastFailureTicks);
            return elapsed >= cooldown;
        }
    }
}
