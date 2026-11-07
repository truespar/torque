package com.truespar.torque.http;

import java.net.URI;
import java.time.Duration;
import java.time.Instant;
import java.util.List;
import java.util.concurrent.atomic.AtomicInteger;
import java.util.concurrent.atomic.AtomicReferenceArray;

/**
 * Tracks health state of nodes for round-robin load distribution and failover.
 *
 * <p>Nodes start healthy. When a request to a node fails (network error or 5xx),
 * the node is marked unhealthy and skipped in the rotation until the healthcheck
 * interval elapses, at which point it becomes eligible again.
 *
 * <p>This class is thread-safe. All state mutations use atomic operations.
 */
final class NodeHealthTracker {

    private final List<URI> nodeUris;
    private final Duration healthcheckInterval;
    private final AtomicInteger roundRobinIndex;
    private final AtomicReferenceArray<Instant> lastFailure;

    /**
     * Creates a health tracker for the given node URIs.
     *
     * @param nodeUris             list of base URIs for each node
     * @param healthcheckInterval  duration after which an unhealthy node is retried
     */
    NodeHealthTracker(List<URI> nodeUris, Duration healthcheckInterval) {
        this.nodeUris = nodeUris;
        this.healthcheckInterval = healthcheckInterval;
        this.roundRobinIndex = new AtomicInteger(0);
        this.lastFailure = new AtomicReferenceArray<>(nodeUris.size());
    }

    /**
     * Returns the number of configured nodes.
     *
     * @return node count
     */
    int size() {
        return nodeUris.size();
    }

    /**
     * Returns the next healthy node URI using round-robin selection.
     *
     * <p>Skips nodes that are marked unhealthy and whose healthcheck interval
     * has not yet elapsed. If all nodes are unhealthy, returns the least-recently
     * failed node as a last resort.
     *
     * @return the base URI of the selected node
     */
    URI nextNode() {
        int size = nodeUris.size();
        if (size == 1) return nodeUris.getFirst();

        var now = Instant.now();
        int startIdx = roundRobinIndex.getAndUpdate(i -> (i + 1) % size);

        // First pass: find a healthy node
        for (int i = 0; i < size; i++) {
            int idx = (startIdx + i) % size;
            if (isHealthy(idx, now)) {
                return nodeUris.get(idx);
            }
        }

        // All unhealthy: pick the one that failed longest ago
        int bestIdx = 0;
        Instant oldest = Instant.MAX;
        for (int i = 0; i < size; i++) {
            var failure = lastFailure.get(i);
            if (failure != null && failure.isBefore(oldest)) {
                oldest = failure;
                bestIdx = i;
            }
        }
        return nodeUris.get(bestIdx);
    }

    /**
     * Returns the node URI at the given index.
     *
     * @param index node index
     * @return the base URI
     */
    URI nodeAt(int index) {
        return nodeUris.get(index);
    }

    /**
     * Marks a node as unhealthy after a failed request.
     *
     * @param nodeUri the URI of the failed node
     */
    void markUnhealthy(URI nodeUri) {
        int idx = nodeUris.indexOf(nodeUri);
        if (idx >= 0) {
            lastFailure.set(idx, Instant.now());
        }
    }

    /**
     * Marks a node as healthy after a successful request.
     *
     * @param nodeUri the URI of the recovered node
     */
    void markHealthy(URI nodeUri) {
        int idx = nodeUris.indexOf(nodeUri);
        if (idx >= 0) {
            lastFailure.set(idx, null);
        }
    }

    private boolean isHealthy(int idx, Instant now) {
        var failure = lastFailure.get(idx);
        if (failure == null) return true;
        return Duration.between(failure, now).compareTo(healthcheckInterval) >= 0;
    }
}
