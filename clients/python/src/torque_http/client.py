"""Torque Python HTTP client — zero external dependencies.

Uses only Python 3.10+ stdlib: http.client, json, ssl, threading.
Features: connection pooling with keep-alive, multi-node round-robin,
configurable retry with exponential backoff, typed error hierarchy.
"""

from __future__ import annotations

import base64
import hashlib
import hmac
import http.client
import json
import ssl
import threading
import time
from dataclasses import dataclass, field
from typing import Any, Iterator
from urllib.parse import quote, urlencode, urlparse

from .exceptions import (
    RequestTimeout,
    TorqueApiError,
    TorqueApiException,
    TorqueError,
    raise_for_status,
)
from .models import (
    AdminStats,
    ApiKey,
    CollectionAlias,
    CollectionResponse,
    CollectionSchema,
    DeleteResult,
    FacetCount,
    FacetResult,
    GroupedHit,
    GroupedSearchResult,
    HealthResponse,
    Hit,
    ImportResult,
    Preset,
    SearchParameters,
    SearchResult,
    StopwordSet,
    SynonymItem,
    SynonymSet,
    UpdateResult,
)
from .tcp import DocumentEncoder, TorqueBinaryFileWriter


# ── Node health tracking ─────────────────────────────────────────────

@dataclass
class Node:
    """A single server node with health tracking."""
    host: str
    port: int
    scheme: str = "http"
    path_prefix: str = ""
    _healthy: bool = True
    _last_failure: float = 0.0

    def mark_unhealthy(self) -> None:
        self._healthy = False
        self._last_failure = time.monotonic()

    def is_healthy(self, healthcheck_interval: float) -> bool:
        if self._healthy:
            return True
        if time.monotonic() - self._last_failure >= healthcheck_interval:
            self._healthy = True
            return True
        return False


# ── Connection pool ──────────────────────────────────────────────────

class _ConnectionPool:
    """Thread-safe pool of persistent HTTP(S) connections per node.

    Each node gets a pool of keep-alive connections. Connections are
    reused across requests to avoid TCP handshake overhead.
    """

    def __init__(
        self,
        *,
        max_connections_per_node: int = 8,
        timeout: float = 30.0,
        ssl_context: ssl.SSLContext | None = None,
    ) -> None:
        self._max = max_connections_per_node
        self._timeout = timeout
        self._ssl_context = ssl_context
        self._lock = threading.Lock()
        # Key: (scheme, host, port) -> list of idle connections
        self._pools: dict[tuple[str, str, int], list[http.client.HTTPConnection]] = {}

    def acquire(self, node: Node) -> http.client.HTTPConnection:
        """Get a connection from the pool, or create a new one.

        Args:
            node: The target server node.

        Returns:
            An HTTPConnection (or HTTPSConnection) ready for use.
        """
        key = (node.scheme, node.host, node.port)
        with self._lock:
            pool = self._pools.get(key, [])
            if pool:
                conn = pool.pop()
                return conn
        return self._create(node)

    def release(self, node: Node, conn: http.client.HTTPConnection) -> None:
        """Return a connection to the pool for reuse.

        Args:
            node: The node this connection belongs to.
            conn: The connection to return.
        """
        key = (node.scheme, node.host, node.port)
        with self._lock:
            pool = self._pools.setdefault(key, [])
            if len(pool) < self._max:
                pool.append(conn)
                return
        # Pool is full, close the connection
        try:
            conn.close()
        except Exception:
            pass

    def discard(self, conn: http.client.HTTPConnection) -> None:
        """Close and discard a broken connection.

        Args:
            conn: The connection to discard.
        """
        try:
            conn.close()
        except Exception:
            pass

    def close_all(self) -> None:
        """Close all pooled connections."""
        with self._lock:
            for pool in self._pools.values():
                for conn in pool:
                    try:
                        conn.close()
                    except Exception:
                        pass
            self._pools.clear()

    def _create(self, node: Node) -> http.client.HTTPConnection:
        if node.scheme == "https":
            ctx = self._ssl_context or ssl.create_default_context()
            conn = http.client.HTTPSConnection(
                node.host, node.port, timeout=self._timeout, context=ctx,
            )
        else:
            conn = http.client.HTTPConnection(
                node.host, node.port, timeout=self._timeout,
            )
        return conn


# ── Client ───────────────────────────────────────────────────────────

class TorqueHttpClient:
    """Torque search engine HTTP client.

    Supports the full Typesense v30.1 HTTP API plus Torque-specific
    binary ingest and TQBF file upload. Zero external dependencies.

    Features:
        - Connection pooling with HTTP keep-alive (persistent connections)
        - Multi-node support with round-robin load balancing
        - Automatic retry with exponential backoff
        - Typed exception hierarchy for precise error handling
        - Scoped API key generation via HMAC

    Usage::

        # Single node
        client = TorqueHttpClient("http://localhost:8108", api_key="your-key")
        results = client.search("products", q="laptop", query_by="title")

        # Multi-node with custom retry
        client = TorqueHttpClient(
            ["http://node1:8108", "http://node2:8108", "http://node3:8108"],
            api_key="your-key",
            num_retries=5,
            retry_interval=0.5,
        )

        # With typed search parameters
        from torque_http import SearchParameters
        params = SearchParameters(q="laptop", query_by="title", per_page=20)
        results = client.search("products", **params)
    """

    def __init__(
        self,
        url: str | list[str],
        *,
        api_key: str,
        timeout: float = 30.0,
        transfer_timeout: float = 300.0,
        num_retries: int = 3,
        retry_interval: float = 0.1,
        healthcheck_interval: float = 60.0,
        max_connections_per_node: int = 8,
        allow_untrusted_certificate: bool = False,
        send_api_key_as_query_param: bool = False,
    ) -> None:
        """Initialize the Torque client.

        Args:
            url: Server URL or list of URLs for multi-node round-robin.
                Accepts ``"http://host:port"`` or a list of such strings.
            api_key: API key for authentication (X-TYPESENSE-API-KEY header).
            timeout: Default request timeout in seconds.
            transfer_timeout: Timeout for bulk transfer operations (import/export).
            num_retries: Number of retry attempts on transient failures (default 3).
            retry_interval: Base interval between retries in seconds. Actual delay
                uses exponential backoff: ``retry_interval * 2^attempt``.
            healthcheck_interval: Seconds before retrying an unhealthy node.
            max_connections_per_node: Maximum pooled connections per node.
            allow_untrusted_certificate: Skip TLS certificate verification.
            send_api_key_as_query_param: When True, append the API key as
                ``x-typesense-api-key`` query parameter instead of using the
                ``X-TYPESENSE-API-KEY`` header.
        """
        self._api_key = api_key
        self._send_api_key_as_query_param = send_api_key_as_query_param
        self._timeout = timeout
        self._transfer_timeout = transfer_timeout
        self._num_retries = num_retries
        self._retry_interval = retry_interval
        self._healthcheck_interval = healthcheck_interval

        # SSL context
        ssl_ctx: ssl.SSLContext | None = None
        if allow_untrusted_certificate:
            ssl_ctx = ssl.create_default_context()
            ssl_ctx.check_hostname = False
            ssl_ctx.verify_mode = ssl.CERT_NONE

        # Parse node URLs
        urls = [url] if isinstance(url, str) else url
        if not urls:
            raise ValueError("At least one server URL is required")
        self._nodes: list[Node] = []
        for u in urls:
            parsed = urlparse(u)
            scheme = parsed.scheme or "http"
            host = parsed.hostname or "localhost"
            port = parsed.port or (443 if scheme == "https" else 8108)
            path_prefix = (parsed.path or "").rstrip("/")
            self._nodes.append(Node(
                host=host, port=port, scheme=scheme, path_prefix=path_prefix,
            ))

        # Round-robin counter
        self._node_index = 0
        self._node_lock = threading.Lock()

        # Connection pool
        self._pool = _ConnectionPool(
            max_connections_per_node=max_connections_per_node,
            timeout=timeout,
            ssl_context=ssl_ctx,
        )

        # Backward compat: expose single-node base_url
        first = self._nodes[0]
        self._base_url = f"{first.scheme}://{first.host}:{first.port}{first.path_prefix}"

    def __enter__(self) -> TorqueHttpClient:
        return self

    def __exit__(self, *args: Any) -> None:
        self.close()

    def close(self) -> None:
        """Close all pooled connections.

        Called automatically when using the client as a context manager.
        """
        self._pool.close_all()

    # ── Health ──────────────────────────────────────────────────────────

    def health(self) -> HealthResponse:
        """Get server health status.

        Returns:
            HealthResponse with server version.
        """
        d = self._get("/health")
        return HealthResponse(
            ok=d.get("ok", False),
            version=d.get("version", ""),
        )

    def stats(self) -> AdminStats:
        """Get server admin statistics.

        Returns:
            AdminStats with system info and per-collection stats.
        """
        d = self._get("/admin/stats")
        return AdminStats(
            system=d.get("system", {}),
            collections=d.get("collections", []),
        )

    def metrics(self) -> str:
        """Get Prometheus metrics.

        Returns:
            Raw Prometheus text metrics.
        """
        return self._get_text("/metrics")

    # ── Collections ────────────────────────────────────────────────────

    def create_collection(self, schema: CollectionSchema) -> CollectionResponse:
        """Create a new collection.

        Args:
            schema: Collection schema with name, fields, and options.

        Returns:
            CollectionResponse with the created collection metadata.

        Raises:
            ObjectAlreadyExists: If a collection with that name exists.
        """
        d = self._post("/collections", schema.to_dict())
        return _parse_collection(d)

    def list_collections(self) -> list[CollectionResponse]:
        """List all collections.

        Returns:
            List of CollectionResponse objects.
        """
        return [_parse_collection(c) for c in self._get("/collections")]

    def get_collection(self, name: str) -> CollectionResponse:
        """Get collection details.

        Args:
            name: Collection name.

        Returns:
            CollectionResponse with metadata including document count.

        Raises:
            ObjectNotFound: If the collection does not exist.
        """
        return _parse_collection(self._get(f"/collections/{_enc(name)}"))

    def update_collection(self, name: str, schema: dict[str, Any]) -> CollectionResponse:
        """Update a collection schema (add/drop fields).

        Args:
            name: Collection name to update.
            schema: Partial schema update. Typically contains a ``fields`` list
                with field definitions to add (new fields) or drop
                (``{"name": "field", "drop": True}``).

        Returns:
            CollectionResponse with the updated collection metadata.

        Raises:
            ObjectNotFound: If the collection does not exist.
            RequestMalformed: If the schema update is invalid.
        """
        d = self._patch(f"/collections/{_enc(name)}", schema)
        return _parse_collection(d)

    def delete_collection(self, name: str) -> None:
        """Delete a collection.

        Args:
            name: Collection name to delete.

        Raises:
            ObjectNotFound: If the collection does not exist.
        """
        self._delete(f"/collections/{_enc(name)}")

    # ── Documents ──────────────────────────────────────────────────────

    def create_document(
        self, collection: str, document: dict[str, Any], *, dirty_values: str | None = None,
    ) -> dict[str, Any]:
        """Create a single document.

        Args:
            collection: Collection name.
            document: Document dict with an ``id`` field.
            dirty_values: How to handle dirty values — ``"coerce_or_reject"``,
                ``"coerce_or_drop"``, ``"drop"``, or ``"reject"`` (default).

        Returns:
            The created document as returned by the server.

        Raises:
            ObjectAlreadyExists: If a document with that ID already exists.
        """
        qs = f"?dirty_values={_enc(dirty_values)}" if dirty_values else ""
        return self._post(f"/collections/{_enc(collection)}/documents{qs}", document)

    def upsert_document(
        self, collection: str, document: dict[str, Any], *, dirty_values: str | None = None,
    ) -> dict[str, Any]:
        """Create or update a single document.

        If a document with the same ``id`` exists it is replaced; otherwise
        a new document is created.

        Args:
            collection: Collection name.
            document: Document dict with an ``id`` field.
            dirty_values: How to handle dirty values — ``"coerce_or_reject"``,
                ``"coerce_or_drop"``, ``"drop"``, or ``"reject"`` (default).

        Returns:
            The upserted document as returned by the server.
        """
        qs = "action=upsert"
        if dirty_values:
            qs += f"&dirty_values={_enc(dirty_values)}"
        return self._post(
            f"/collections/{_enc(collection)}/documents?{qs}",
            document,
        )

    def get_document(self, collection: str, document_id: str) -> dict[str, Any]:
        """Get a document by ID.

        Args:
            collection: Collection name.
            document_id: Document ID.

        Returns:
            The document dict.

        Raises:
            ObjectNotFound: If the document does not exist.
        """
        return self._get(f"/collections/{_enc(collection)}/documents/{_enc(document_id)}")

    def update_document(self, collection: str, document_id: str, fields: dict[str, Any]) -> dict[str, Any]:
        """Partially update a document.

        Args:
            collection: Collection name.
            document_id: Document ID.
            fields: Dict of fields to update.

        Returns:
            The updated document dict.

        Raises:
            ObjectNotFound: If the document does not exist.
        """
        return self._patch(f"/collections/{_enc(collection)}/documents/{_enc(document_id)}", fields)

    def delete_document(self, collection: str, document_id: str) -> None:
        """Delete a document by ID.

        Args:
            collection: Collection name.
            document_id: Document ID.

        Raises:
            ObjectNotFound: If the document does not exist.
        """
        self._delete(f"/collections/{_enc(collection)}/documents/{_enc(document_id)}")

    def delete_by_filter(self, collection: str, filter_by: str) -> DeleteResult:
        """Delete documents matching a filter.

        Args:
            collection: Collection name.
            filter_by: Typesense filter expression (e.g. ``"status:=inactive"``).

        Returns:
            DeleteResult with the count of deleted documents.
        """
        d = self._delete_json(f"/collections/{_enc(collection)}/documents?filter_by={quote(filter_by)}")
        return DeleteResult(num_deleted=d.get("num_deleted", 0))

    def delete_documents_by_filter(self, collection: str, filter_by: str) -> DeleteResult:
        """Delete documents matching a filter.

        Alias for ``delete_by_filter`` with a more explicit name.

        Args:
            collection: Collection name.
            filter_by: Typesense filter expression.

        Returns:
            DeleteResult with the count of deleted documents.
        """
        return self.delete_by_filter(collection, filter_by)

    def truncate_collection(self, collection: str) -> DeleteResult:
        """Delete all documents from a collection while keeping its schema.

        Args:
            collection: Collection name.

        Returns:
            DeleteResult with the count of deleted documents.
        """
        d = self._delete_json(f"/collections/{_enc(collection)}/documents")
        return DeleteResult(num_deleted=d.get("num_deleted", 0))

    def update_documents_by_filter(
        self, collection: str, document: dict[str, Any], filter_by: str,
    ) -> UpdateResult:
        """Update documents matching a filter.

        Args:
            collection: Collection name.
            document: Dict of fields to update on all matching documents.
            filter_by: Typesense filter expression.

        Returns:
            UpdateResult with the count of updated documents.
        """
        d = self._patch(
            f"/collections/{_enc(collection)}/documents?filter_by={quote(filter_by)}",
            document,
        )
        return UpdateResult(num_updated=d.get("num_updated", 0))

    def import_documents(
        self,
        collection: str,
        documents: list[dict[str, Any]],
        *,
        action: str = "upsert",
        dirty_values: str | None = None,
        batch_size: int | None = None,
        return_doc: bool = False,
        return_id: bool = False,
    ) -> list[ImportResult]:
        """Import documents via JSONL.

        Args:
            collection: Collection name.
            documents: List of document dicts.
            action: Import action — ``"create"``, ``"upsert"``, ``"update"``,
                or ``"emplace"`` (insert if missing, update existing fields).
            dirty_values: How to handle dirty values — ``"coerce_or_reject"``,
                ``"coerce_or_drop"``, ``"drop"``, or ``"reject"`` (default).
            batch_size: When set, automatically chunks the list and sends
                multiple requests of this size. Results are concatenated.
            return_doc: Pass ``return_doc=true`` query param so the server
                echoes back the full document in each ImportResult.
            return_id: Pass ``return_id=true`` query param so the server
                includes the document ``id`` in each ImportResult.

        Returns:
            List of ImportResult, one per document.
        """
        if batch_size is not None and batch_size > 0 and len(documents) > batch_size:
            results: list[ImportResult] = []
            for i in range(0, len(documents), batch_size):
                chunk = documents[i:i + batch_size]
                results.extend(self.import_documents(
                    collection, chunk,
                    action=action,
                    dirty_values=dirty_values,
                    return_doc=return_doc,
                    return_id=return_id,
                ))
            return results

        qs = f"action={_enc(action)}"
        if dirty_values:
            qs += f"&dirty_values={_enc(dirty_values)}"
        if return_doc:
            qs += "&return_doc=true"
        if return_id:
            qs += "&return_id=true"
        body = "\n".join(json.dumps(doc, separators=(",", ":")) for doc in documents)
        text = self._post_text(
            f"/collections/{_enc(collection)}/documents/import?{qs}",
            body.encode("utf-8"),
            content_type="text/plain",
        )
        results = []
        for line in text.strip().split("\n"):
            if not line:
                continue
            d = json.loads(line)
            results.append(ImportResult(
                success=d.get("success", False),
                error=d.get("error"),
                id=d.get("id"),
            ))
        return results

    def import_jsonl(
        self, collection: str, jsonl: str, *, action: str = "upsert", dirty_values: str | None = None,
    ) -> list[ImportResult]:
        """Import raw JSONL string.

        Args:
            collection: Collection name.
            jsonl: Newline-delimited JSON string.
            action: Import action — ``"create"``, ``"upsert"``, ``"update"``,
                or ``"emplace"``.
            dirty_values: How to handle dirty values — ``"coerce_or_reject"``,
                ``"coerce_or_drop"``, ``"drop"``, or ``"reject"`` (default).

        Returns:
            List of ImportResult, one per document.
        """
        qs = f"action={_enc(action)}"
        if dirty_values:
            qs += f"&dirty_values={_enc(dirty_values)}"
        text = self._post_text(
            f"/collections/{_enc(collection)}/documents/import?{qs}",
            jsonl.encode("utf-8"),
            content_type="text/plain",
        )
        results = []
        for line in text.strip().split("\n"):
            if line:
                d = json.loads(line)
                results.append(ImportResult(success=d.get("success", False), error=d.get("error")))
        return results

    def export_documents(
        self,
        collection: str,
        *,
        filter_by: str | None = None,
        include_fields: str | None = None,
        exclude_fields: str | None = None,
    ) -> Iterator[dict[str, Any]]:
        """Export documents as an iterator of dicts.

        Args:
            collection: Collection name.
            filter_by: Optional Typesense filter expression to export a subset.
            include_fields: Comma-separated list of fields to include.
            exclude_fields: Comma-separated list of fields to exclude.

        Yields:
            Document dicts, one per exported document.
        """
        params: dict[str, str] = {}
        if filter_by is not None:
            params["filter_by"] = filter_by
        if include_fields is not None:
            params["include_fields"] = include_fields
        if exclude_fields is not None:
            params["exclude_fields"] = exclude_fields
        qs = f"?{urlencode(params)}" if params else ""
        text = self._get_text(f"/collections/{_enc(collection)}/documents/export{qs}")
        for line in text.strip().split("\n"):
            if line:
                yield json.loads(line)

    def export_documents_stream(
        self,
        collection: str,
        *,
        filter_by: str | None = None,
        include_fields: str | None = None,
        exclude_fields: str | None = None,
    ) -> Iterator[str]:
        """Export documents as a streaming iterator of JSONL lines.

        Unlike ``export_documents``, this streams line-by-line from the
        HTTP response without buffering the entire body in memory.

        Args:
            collection: Collection name.
            filter_by: Optional Typesense filter expression to export a subset.
            include_fields: Comma-separated list of fields to include.
            exclude_fields: Comma-separated list of fields to exclude.

        Yields:
            Raw JSON strings, one per exported document.
        """
        params: dict[str, str] = {}
        if filter_by is not None:
            params["filter_by"] = filter_by
        if include_fields is not None:
            params["include_fields"] = include_fields
        if exclude_fields is not None:
            params["exclude_fields"] = exclude_fields
        qs = f"?{urlencode(params)}" if params else ""
        path = f"/collections/{_enc(collection)}/documents/export{qs}"
        yield from self._request_stream_lines("GET", path)

    def import_documents_stream(
        self,
        collection: str,
        stream: Iterator[str],
        *,
        action: str = "create",
        dirty_values: str | None = None,
    ) -> list[ImportResult]:
        """Import documents from a streaming iterable of JSONL lines.

        Accepts any iterable/generator that yields one JSON string per
        document. Lines are concatenated and sent as a single request body.

        Args:
            collection: Collection name.
            stream: Iterable of JSON strings (one per document).
            action: Import action — ``"create"``, ``"upsert"``, ``"update"``,
                or ``"emplace"``.
            dirty_values: How to handle dirty values.

        Returns:
            List of ImportResult, one per document.
        """
        qs = f"action={_enc(action)}"
        if dirty_values:
            qs += f"&dirty_values={_enc(dirty_values)}"
        body = "\n".join(stream)
        text = self._post_text(
            f"/collections/{_enc(collection)}/documents/import?{qs}",
            body.encode("utf-8"),
            content_type="text/plain",
        )
        results = []
        for line in text.strip().split("\n"):
            if not line:
                continue
            d = json.loads(line)
            results.append(ImportResult(
                success=d.get("success", False),
                error=d.get("error"),
                id=d.get("id"),
            ))
        return results

    def import_binary(self, collection: str, file_path: str) -> dict[str, Any]:
        """Upload a TQBF binary file.

        Args:
            collection: Collection name.
            file_path: Path to a ``.tqbf`` file on disk.

        Returns:
            Server response dict with import stats.
        """
        with open(file_path, "rb") as f:
            data = f.read()
        return self._post_bytes(
            f"/collections/{_enc(collection)}/documents/import-binary",
            data,
            content_type="application/octet-stream",
        )

    # ── Search ─────────────────────────────────────────────────────────

    def search(
        self,
        collection: str,
        params: SearchParameters | dict[str, Any] | None = None,
        **kwargs: Any,
    ) -> SearchResult:
        """Search documents.

        Accepts either a ``SearchParameters`` TypedDict, a plain dict, or
        keyword arguments (for backward compatibility). All three forms can
        be combined — keyword arguments override dict values.

        Args:
            collection: Collection name to search.
            params: Optional typed search parameters dict.
            **kwargs: Additional or overriding search parameters.

        Returns:
            SearchResult with hits, facets, and timing info.

        Examples::

            # Keyword arguments (backward compatible)
            client.search("products", q="laptop", query_by="title")

            # Typed parameters
            from torque_http import SearchParameters
            p = SearchParameters(q="laptop", query_by="title", per_page=20)
            client.search("products", p)

            # Mixed
            client.search("products", p, filter_by="price:>100")
        """
        merged: dict[str, Any] = {}
        if params is not None:
            merged.update(params)
        merged.update(kwargs)
        qs = _encode_search_params(merged)
        d = self._get(f"/collections/{_enc(collection)}/documents/search?{qs}")
        return _parse_search(d)

    def search_grouped(
        self,
        collection: str,
        params: SearchParameters | dict[str, Any] | None = None,
        **kwargs: Any,
    ) -> GroupedSearchResult:
        """Search with group_by -- returns grouped results.

        Args:
            collection: Collection name.
            params: Optional typed search parameters dict (must include ``group_by``).
            **kwargs: Additional or overriding search parameters.

        Returns:
            GroupedSearchResult with grouped hits.
        """
        merged: dict[str, Any] = {}
        if params is not None:
            merged.update(params)
        merged.update(kwargs)
        qs = _encode_search_params(merged)
        d = self._get(f"/collections/{_enc(collection)}/documents/search?{qs}")
        return _parse_grouped_search(d)

    def multi_search(
        self,
        searches: list[dict[str, Any]],
        common_params: dict[str, Any] | None = None,
    ) -> list[SearchResult]:
        """Execute multiple searches in one request.

        Args:
            searches: List of search parameter dicts, each with at least
                ``collection`` and ``q`` keys.
            common_params: Optional dict of parameters merged into every
                search. Per-search values override common values.

        Returns:
            List of SearchResult, one per input search.
        """
        if common_params:
            merged = []
            for s in searches:
                m = {**common_params, **s}
                merged.append(m)
            searches = merged
        d = self._post("/multi_search", {"searches": searches})
        return [_parse_search(r) for r in d.get("results", [])]

    def multi_search_union(
        self,
        searches: list[dict[str, Any]],
        common_params: dict[str, Any] | None = None,
        *,
        page: int | None = None,
        per_page: int | None = None,
    ) -> SearchResult:
        """Execute multiple searches merged into one ranked result set.

        Uses Typesense v30.1 ``union: true`` — hits from all searches are
        merged and ranked by text_match. Not compatible with ``group_by``
        searches.

        Args:
            searches: List of search parameter dicts, each with at least
                ``collection`` and ``q`` keys.
            common_params: Optional dict of parameters merged into every
                search. Per-search values override common values.
            page: Page number for the merged result set (1-based).
            per_page: Results per page for the merged result set.

        Returns:
            A single SearchResult with the merged, ranked hits.
        """
        if common_params:
            searches = [{**common_params, **s} for s in searches]
        path = "/multi_search"
        query: list[str] = []
        if page is not None:
            query.append(f"page={page}")
        if per_page is not None:
            query.append(f"per_page={per_page}")
        if query:
            path += "?" + "&".join(query)
        d = self._post(path, {"searches": searches, "union": True})
        return _parse_search(d)

    # ── Aliases ────────────────────────────────────────────────────────

    def upsert_alias(self, name: str, collection_name: str) -> CollectionAlias:
        """Create or update a collection alias.

        Args:
            name: Alias name.
            collection_name: Target collection name.

        Returns:
            CollectionAlias with the alias mapping.
        """
        d = self._put(f"/aliases/{_enc(name)}", {"collection_name": collection_name})
        return CollectionAlias(name=d.get("name", ""), collection_name=d.get("collection_name", ""))

    def get_alias(self, name: str) -> CollectionAlias:
        """Get an alias.

        Args:
            name: Alias name.

        Returns:
            CollectionAlias with the alias mapping.

        Raises:
            ObjectNotFound: If the alias does not exist.
        """
        d = self._get(f"/aliases/{_enc(name)}")
        return CollectionAlias(name=d.get("name", ""), collection_name=d.get("collection_name", ""))

    def list_aliases(self) -> list[CollectionAlias]:
        """List all aliases.

        Returns:
            List of CollectionAlias objects.
        """
        d = self._get("/aliases")
        return [CollectionAlias(name=a.get("name", ""), collection_name=a.get("collection_name", ""))
                for a in d.get("aliases", [])]

    def delete_alias(self, name: str) -> None:
        """Delete an alias.

        Args:
            name: Alias name.

        Raises:
            ObjectNotFound: If the alias does not exist.
        """
        self._delete(f"/aliases/{_enc(name)}")

    # ── API Keys ───────────────────────────────────────────────────────

    def create_key(self, description: str, actions: list[str], collections: list[str],
                   *, expires_at: int | None = None) -> ApiKey:
        """Create an API key.

        Args:
            description: Human-readable description for the key.
            actions: List of allowed actions (e.g. ``["documents:search"]``).
            collections: List of allowed collection names (``["*"]`` for all).
            expires_at: Optional Unix timestamp when the key expires.

        Returns:
            ApiKey with the generated key value (only visible on creation).
        """
        body: dict[str, Any] = {"description": description, "actions": actions, "collections": collections}
        if expires_at is not None:
            body["expires_at"] = expires_at
        d = self._post("/keys", body)
        return _parse_key(d)

    def list_keys(self) -> list[ApiKey]:
        """List all API keys.

        Returns:
            List of ApiKey objects (``value`` is redacted).
        """
        d = self._get("/keys")
        return [_parse_key(k) for k in d.get("keys", [])]

    def get_key(self, key_id: int) -> ApiKey:
        """Get an API key by ID.

        Args:
            key_id: Numeric key ID.

        Returns:
            ApiKey with metadata (``value`` is redacted).

        Raises:
            ObjectNotFound: If the key does not exist.
        """
        return _parse_key(self._get(f"/keys/{key_id}"))

    def delete_key(self, key_id: int) -> None:
        """Delete an API key.

        Args:
            key_id: Numeric key ID.

        Raises:
            ObjectNotFound: If the key does not exist.
        """
        self._delete(f"/keys/{key_id}")

    def generate_scoped_search_key(
        self,
        search_key: str,
        params: dict[str, Any],
    ) -> str:
        """Generate an HMAC-based scoped search key.

        Scoped keys embed search restrictions (filter_by, expires_at, etc.)
        that cannot be modified by the client. The key is derived from a
        parent search-only API key using HMAC-SHA256.

        This matches the Typesense v30.1 scoped key format.

        Args:
            search_key: The parent search-only API key value.
            params: Dict of embedded search parameters. Common fields:
                ``filter_by``, ``expires_at``, ``collection``.

        Returns:
            Base64-encoded scoped key string suitable for use as an API key.

        Example::

            scoped = client.generate_scoped_search_key(
                "search-only-key-value",
                {"filter_by": "company_id:=123", "expires_at": 1700000000},
            )
        """
        params_json = json.dumps(params, separators=(",", ":"))
        digest = hmac.new(
            search_key.encode("utf-8"),
            params_json.encode("utf-8"),
            hashlib.sha256,
        ).digest()
        digest_b64 = base64.b64encode(digest).decode("utf-8")
        prefix = search_key[:4]
        raw = f"{digest_b64}{prefix}{params_json}"
        return base64.b64encode(raw.encode("utf-8")).decode("utf-8")

    # ── Synonyms ───────────────────────────────────────────────────────

    def upsert_synonym_set(self, name: str, items: list[dict[str, Any]] | None = None) -> SynonymSet:
        """Create or update a synonym set.

        Args:
            name: Synonym set name.
            items: List of synonym rule dicts.

        Returns:
            SynonymSet with the set name.
        """
        body: dict[str, Any] = {"name": name, "items": items or []}
        d = self._put(f"/synonym_sets/{_enc(name)}", body)
        return SynonymSet(name=d.get("name", ""), items=[])

    def list_synonym_sets(self) -> list[SynonymSet]:
        """List all synonym sets.

        Returns:
            List of SynonymSet objects.
        """
        d = self._get("/synonym_sets")
        return [SynonymSet(name=s.get("name", "")) for s in d.get("synonym_sets", [])]

    def get_synonym_set(self, name: str) -> SynonymSet:
        """Get a synonym set by name.

        Args:
            name: Synonym set name.

        Returns:
            SynonymSet with name and items.

        Raises:
            ObjectNotFound: If the synonym set does not exist.
        """
        d = self._get(f"/synonym_sets/{_enc(name)}")
        items = [
            SynonymItem(
                id=item.get("id", ""),
                synonyms=item.get("synonyms", []),
                root=item.get("root"),
                locale=item.get("locale"),
            )
            for item in d.get("items", [])
        ]
        return SynonymSet(name=d.get("name", ""), items=items)

    def delete_synonym_set(self, name: str) -> None:
        """Delete a synonym set.

        Args:
            name: Synonym set name.

        Raises:
            ObjectNotFound: If the synonym set does not exist.
        """
        self._delete(f"/synonym_sets/{_enc(name)}")

    # ── Presets ────────────────────────────────────────────────────────

    def upsert_preset(self, preset_id: str, value: dict[str, Any]) -> Preset:
        """Create or update a search preset.

        Args:
            preset_id: Preset identifier.
            value: Dict of search parameters to save.

        Returns:
            Preset with the ID and saved value.
        """
        d = self._put(f"/presets/{_enc(preset_id)}", {"id": preset_id, "value": value})
        return Preset(id=d.get("id", ""), value=d.get("value", {}))

    def list_presets(self) -> list[Preset]:
        """List all presets.

        Returns:
            List of Preset objects.
        """
        d = self._get("/presets")
        return [Preset(id=p.get("id", ""), value=p.get("value", {})) for p in d.get("presets", [])]

    def get_preset(self, preset_id: str) -> Preset:
        """Get a preset by ID.

        Args:
            preset_id: Preset identifier.

        Returns:
            Preset with the ID and saved value.

        Raises:
            ObjectNotFound: If the preset does not exist.
        """
        d = self._get(f"/presets/{_enc(preset_id)}")
        return Preset(id=d.get("id", ""), value=d.get("value", {}))

    def delete_preset(self, preset_id: str) -> None:
        """Delete a preset.

        Args:
            preset_id: Preset identifier.

        Raises:
            ObjectNotFound: If the preset does not exist.
        """
        self._delete(f"/presets/{_enc(preset_id)}")

    # ── Stopwords ──────────────────────────────────────────────────────

    def upsert_stopwords(self, stopword_id: str, stopwords: list[str]) -> StopwordSet:
        """Create or update a stopword set.

        Args:
            stopword_id: Stopword set identifier.
            stopwords: List of stopword strings.

        Returns:
            StopwordSet with the ID and word list.
        """
        d = self._put(f"/stopwords/{_enc(stopword_id)}", {"id": stopword_id, "stopwords": stopwords})
        return StopwordSet(id=d.get("id", ""), stopwords=d.get("stopwords", []))

    def list_stopwords(self) -> list[StopwordSet]:
        """List all stopword sets.

        Returns:
            List of StopwordSet objects.
        """
        d = self._get("/stopwords")
        return [StopwordSet(id=s.get("id", ""), stopwords=s.get("stopwords", []))
                for s in d.get("stopwords", [])]

    def get_stopwords_set(self, stopword_id: str) -> StopwordSet:
        """Get a stopword set by ID.

        Args:
            stopword_id: Stopword set identifier.

        Returns:
            StopwordSet with the ID and word list.

        Raises:
            ObjectNotFound: If the stopword set does not exist.
        """
        d = self._get(f"/stopwords/{_enc(stopword_id)}")
        return StopwordSet(id=d.get("id", ""), stopwords=d.get("stopwords", []))

    def delete_stopwords(self, stopword_id: str) -> None:
        """Delete a stopword set.

        Args:
            stopword_id: Stopword set identifier.

        Raises:
            ObjectNotFound: If the stopword set does not exist.
        """
        self._delete(f"/stopwords/{_enc(stopword_id)}")

    # ── TQBF Binary File Writer ────────────────────────────────────────

    def create_binary_writer(self, file_path: str, schema: dict[str, Any]) -> TorqueBinaryFileWriter:
        """Create a streaming binary file writer for TQBF format.

        Uses proper binary encoding via DocumentEncoder (not JSONL).

        Args:
            file_path: Output file path for the ``.tqbf`` file.
            schema: Collection schema dict with a ``fields`` list.

        Returns:
            TorqueBinaryFileWriter context manager for streaming writes.

        Example::

            schema = {"fields": [{"name": "title", "type": "string"}, ...]}
            with client.create_binary_writer("data.tqbf", schema) as writer:
                for doc in data_source:
                    writer.write(doc)
            client.import_binary("my_collection", "data.tqbf")
        """
        encoder = DocumentEncoder(schema)
        return TorqueBinaryFileWriter(file_path, encoder)

    # ── Internal HTTP ──────────────────────────────────────────────────

    def _next_node(self) -> Node:
        """Select the next healthy node via round-robin."""
        with self._node_lock:
            start = self._node_index
            for _ in range(len(self._nodes)):
                node = self._nodes[self._node_index % len(self._nodes)]
                self._node_index = (self._node_index + 1) % len(self._nodes)
                if node.is_healthy(self._healthcheck_interval):
                    return node
            # All nodes unhealthy — reset and return the next one anyway
            for node in self._nodes:
                node._healthy = True
            node = self._nodes[start % len(self._nodes)]
            self._node_index = (start + 1) % len(self._nodes)
            return node

    def _request(
        self,
        method: str,
        path: str,
        body: bytes | None = None,
        content_type: str = "application/json",
        timeout: float | None = None,
    ) -> bytes:
        """Execute an HTTP request with retry and node failover.

        Args:
            method: HTTP method (GET, POST, PUT, PATCH, DELETE).
            path: Request path (e.g. ``/collections``).
            body: Optional request body bytes.
            content_type: Content-Type header value.
            timeout: Request timeout override.

        Returns:
            Response body bytes.

        Raises:
            TorqueApiError: On non-2xx response (typed by status code).
            RequestTimeout: On connection or read timeout.
        """
        effective_timeout = timeout or self._timeout
        last_error: Exception | None = None

        for attempt in range(self._num_retries + 1):
            node = self._next_node()
            if self._send_api_key_as_query_param:
                sep = "&" if "?" in path else "?"
                req_path = f"{node.path_prefix}{path}{sep}x-typesense-api-key={quote(self._api_key, safe='')}"
                headers: dict[str, str] = {}
            else:
                req_path = f"{node.path_prefix}{path}"
                headers = {"X-TYPESENSE-API-KEY": self._api_key}
            conn = self._pool.acquire(node)

            # Set timeout for this request
            conn.timeout = effective_timeout

            try:
                if body is not None:
                    headers["Content-Type"] = content_type

                conn.request(method, req_path, body=body, headers=headers)
                resp = conn.getresponse()
                resp_body = resp.read()

                if 200 <= resp.status < 300:
                    self._pool.release(node, conn)
                    return resp_body

                # Non-2xx response — parse error and raise typed exception
                self._pool.release(node, conn)
                msg = None
                try:
                    d = json.loads(resp_body)
                    msg = d.get("message") or d.get("error")
                except Exception:
                    pass
                if msg is None:
                    msg = f"HTTP {resp.status}: {resp_body[:200].decode(errors='replace')}"

                # 5xx and 503 are retryable
                if resp.status >= 500 and attempt < self._num_retries:
                    node.mark_unhealthy()
                    last_error = TorqueApiError(msg, status_code=resp.status)
                    time.sleep(self._retry_interval * (2 ** attempt))
                    continue

                raise_for_status(resp.status, msg)

            except TorqueApiError:
                raise
            except TorqueError:
                raise
            except (OSError, http.client.HTTPException, TimeoutError) as e:
                # Connection-level failure — discard and retry
                self._pool.discard(conn)
                node.mark_unhealthy()
                last_error = e

                if attempt < self._num_retries:
                    time.sleep(self._retry_interval * (2 ** attempt))
                    continue

                if isinstance(e, (TimeoutError, OSError)) and "timed out" in str(e).lower():
                    raise RequestTimeout(f"Request to {node.host}:{node.port} timed out after {effective_timeout}s") from e
                raise TorqueError(f"Connection error after {self._num_retries + 1} attempts: {e}") from e

        # Should not reach here, but just in case
        if last_error is not None:
            raise TorqueError(f"All {self._num_retries + 1} attempts failed") from last_error
        raise TorqueError("Request failed with no error details")

    def _get(self, path: str) -> Any:
        return json.loads(self._request("GET", path))

    def _get_text(self, path: str) -> str:
        return self._request("GET", path).decode("utf-8")

    def _request_stream_lines(self, method: str, path: str) -> Iterator[str]:
        """Execute an HTTP request and yield response lines without buffering.

        Uses the transfer timeout and reads the response line-by-line via
        the http.client socket, avoiding full-body buffering for large exports.
        """
        node = self._next_node()
        if self._send_api_key_as_query_param:
            sep = "&" if "?" in path else "?"
            full_path = f"{node.path_prefix}{path}{sep}x-typesense-api-key={quote(self._api_key, safe='')}"
            stream_headers: dict[str, str] = {}
        else:
            full_path = f"{node.path_prefix}{path}"
            stream_headers = {"X-TYPESENSE-API-KEY": self._api_key}
        conn = self._pool.acquire(node)
        conn.timeout = self._transfer_timeout

        try:
            conn.request(method, full_path, headers=stream_headers)
            resp = conn.getresponse()

            if resp.status < 200 or resp.status >= 300:
                body = resp.read()
                self._pool.release(node, conn)
                msg = None
                try:
                    d = json.loads(body)
                    msg = d.get("message") or d.get("error")
                except Exception:
                    pass
                if msg is None:
                    msg = f"HTTP {resp.status}: {body[:200].decode(errors='replace')}"
                raise_for_status(resp.status, msg)

            buf = b""
            while True:
                chunk = resp.read(8192)
                if not chunk:
                    break
                buf += chunk
                while b"\n" in buf:
                    line_bytes, buf = buf.split(b"\n", 1)
                    line = line_bytes.decode("utf-8").strip()
                    if line:
                        yield line
            if buf:
                line = buf.decode("utf-8").strip()
                if line:
                    yield line

            self._pool.release(node, conn)
        except TorqueApiError:
            raise
        except TorqueError:
            raise
        except Exception as e:
            self._pool.discard(conn)
            node.mark_unhealthy()
            raise TorqueError(f"Stream error: {e}") from e

    def _post(self, path: str, body: Any = None) -> Any:
        data = json.dumps(body, separators=(",", ":")).encode("utf-8") if body is not None else None
        resp = self._request("POST", path, data)
        return json.loads(resp) if resp else None

    def _post_text(self, path: str, body: bytes, content_type: str = "text/plain") -> str:
        return self._request("POST", path, body, content_type, self._transfer_timeout).decode("utf-8")

    def _post_bytes(self, path: str, body: bytes, content_type: str = "application/octet-stream") -> Any:
        resp = self._request("POST", path, body, content_type, self._transfer_timeout)
        return json.loads(resp)

    def _put(self, path: str, body: Any) -> Any:
        data = json.dumps(body, separators=(",", ":")).encode("utf-8")
        resp = self._request("PUT", path, data)
        return json.loads(resp) if resp else {}

    def _patch(self, path: str, body: Any) -> Any:
        data = json.dumps(body, separators=(",", ":")).encode("utf-8")
        resp = self._request("PATCH", path, data)
        return json.loads(resp) if resp else {}

    def _delete(self, path: str) -> None:
        self._request("DELETE", path)

    def _delete_json(self, path: str) -> Any:
        resp = self._request("DELETE", path)
        return json.loads(resp) if resp else {}


# ── Helpers ──────────────────────────────────────────────────────────

def _enc(s: str) -> str:
    return quote(s, safe="")


def _encode_search_params(params: dict[str, Any]) -> str:
    """URL-encode search params, serializing Python bools as true/false."""
    return urlencode({
        k: ("true" if v is True else "false" if v is False else v)
        for k, v in params.items()
        if v is not None
    })


def _parse_collection(d: dict) -> CollectionResponse:
    return CollectionResponse(
        name=d.get("name", ""),
        num_documents=d.get("num_documents", 0),
        fields=d.get("fields", []),
        default_sorting_field=d.get("default_sorting_field"),
        enable_nested_fields=d.get("enable_nested_fields", False),
    )


def _parse_search(d: dict) -> SearchResult:
    return SearchResult(
        found=d.get("found", 0),
        out_of=d.get("out_of", 0),
        page=d.get("page", 1),
        per_page=d.get("per_page", 10),
        hits=[Hit(
            document=h.get("document", {}),
            text_match=h.get("text_match", 0),
            highlights=h.get("highlights", []),
        ) for h in d.get("hits", [])],
        facet_counts=[FacetResult(
            field_name=fc.get("field_name", ""),
            counts=[FacetCount(value=c.get("value", ""), count=c.get("count", 0))
                    for c in fc.get("counts", [])],
        ) for fc in d.get("facet_counts", [])],
        search_time_ms=d.get("search_time_ms", 0),
        search_time_us=d.get("search_time_us", 0),
    )


def _parse_grouped_search(d: dict) -> GroupedSearchResult:
    return GroupedSearchResult(
        found=d.get("found", 0),
        out_of=d.get("out_of", 0),
        grouped_hits=[GroupedHit(
            group_key=g.get("group_key", []),
            hits=[Hit(document=h.get("document", {})) for h in g.get("hits", [])],
            found=g.get("found", 0),
        ) for g in d.get("grouped_hits", [])],
        search_time_us=d.get("search_time_us", 0),
    )


def _parse_key(d: dict) -> ApiKey:
    return ApiKey(
        id=d.get("id", 0),
        description=d.get("description", ""),
        actions=d.get("actions", []),
        collections=d.get("collections", []),
        value=d.get("value"),
        expires_at=d.get("expires_at"),
    )


# Backward-compatible alias — deprecated, use TorqueHttpClient instead
