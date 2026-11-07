"""Torque Python HTTP client — zero external dependencies.

Usage:
    from torque_http import TorqueHttpClient

    client = TorqueHttpClient("http://localhost:8108", api_key="your-key")
    results = client.search("products", q="laptop", query_by="title")

Multi-node with failover:
    client = TorqueHttpClient(
        ["http://node1:8108", "http://node2:8108"],
        api_key="your-key",
        num_retries=5,
    )

Typed search parameters:
    from torque_http import SearchParameters
    params = SearchParameters(q="laptop", query_by="title", per_page=20)
    results = client.search("products", params)
"""

from .client import TorqueHttpClient
from .exceptions import (
    TorqueError,
    TorqueApiError,
    TorqueApiException,
    ObjectNotFound,
    ObjectAlreadyExists,
    RequestUnauthorized,
    RequestForbidden,
    RequestMalformed,
    ServerError,
    ServiceUnavailable,
    RequestTimeout,
)
from .models import (
    CollectionAlias,
    CollectionResponse,
    CollectionSchema,
    DeleteResult,
    FieldDefinition,
    HealthResponse,
    ImportResult,
    SearchResult,
    SearchParameters,
    GroupedSearchResult,
    GroupedHit,
    Hit,
    FacetCount,
    FacetResult,
    ApiKey,
    Preset,
    StopwordSet,
    SynonymItem,
    SynonymSet,
    UpdateResult,
    AdminStats,
)
from .tcp import (
    DocumentEncoder,
    TorqueIngestClient,
    TorqueBinaryFileWriter,
    BatchAck,
    CommitResult,
    TorqueProtocolError,
    TorqueConnectionError,
)

__all__ = [
    # Client
    "TorqueHttpClient",
    # Exceptions
    "TorqueError",
    "TorqueApiError",
    "TorqueApiException",
    "ObjectNotFound",
    "ObjectAlreadyExists",
    "RequestUnauthorized",
    "RequestForbidden",
    "RequestMalformed",
    "ServerError",
    "ServiceUnavailable",
    "RequestTimeout",
    # Models — collections
    "CollectionAlias",
    "CollectionResponse",
    "CollectionSchema",
    "FieldDefinition",
    # Models — documents
    "DeleteResult",
    "ImportResult",
    "UpdateResult",
    # Models — search
    "SearchResult",
    "SearchParameters",
    "GroupedSearchResult",
    "GroupedHit",
    "Hit",
    "FacetCount",
    "FacetResult",
    # Models — API keys
    "ApiKey",
    # Models — config
    "Preset",
    "StopwordSet",
    "SynonymItem",
    "SynonymSet",
    # Models — server
    "HealthResponse",
    "AdminStats",
    # TCP ingest
    "DocumentEncoder",
    "TorqueIngestClient",
    "TorqueBinaryFileWriter",
    "BatchAck",
    "CommitResult",
    "TorqueProtocolError",
    "TorqueConnectionError",
]

__version__ = "0.7.0"
