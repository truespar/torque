"""Torque response models — dataclasses, zero dependencies.

Includes typed search parameters for Typesense v30.1 API compatibility.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Any, TypedDict


# ── Collections ──────────────────────────────────────────────────────────

@dataclass
class FieldDefinition:
    """Schema field definition."""
    name: str
    type: str
    facet: bool = False
    index: bool = True
    sort: bool = False
    optional: bool = False
    stem: bool = False
    locale: str | None = None
    num_dim: int | None = None
    vec_dist: str | None = None

    def to_dict(self) -> dict[str, Any]:
        d: dict[str, Any] = {"name": self.name, "type": self.type}
        if self.facet: d["facet"] = True
        if not self.index: d["index"] = False
        if self.sort: d["sort"] = True
        if self.optional: d["optional"] = True
        if self.stem: d["stem"] = True
        if self.locale: d["locale"] = self.locale
        if self.num_dim: d["num_dim"] = self.num_dim
        if self.vec_dist: d["vec_dist"] = self.vec_dist
        return d


@dataclass
class CollectionSchema:
    """Schema for creating a collection."""
    name: str
    fields: list[FieldDefinition]
    default_sorting_field: str | None = None
    enable_nested_fields: bool = False

    def to_dict(self) -> dict[str, Any]:
        d: dict[str, Any] = {
            "name": self.name,
            "fields": [f.to_dict() for f in self.fields],
        }
        if self.default_sorting_field:
            d["default_sorting_field"] = self.default_sorting_field
        if self.enable_nested_fields:
            d["enable_nested_fields"] = True
        return d


@dataclass
class CollectionResponse:
    """Collection metadata returned by the server."""
    name: str
    num_documents: int = 0
    fields: list[dict[str, Any]] = field(default_factory=list)
    default_sorting_field: str | None = None
    enable_nested_fields: bool = False


# ── Documents ────────────────────────────────────────────────────────────

@dataclass
class ImportResult:
    """Result of a single document in a batch import."""
    success: bool
    error: str | None = None
    id: str | None = None


@dataclass
class DeleteResult:
    """Result of a document or batch deletion."""
    num_deleted: int = 0


@dataclass
class UpdateResult:
    """Result of a batch update by filter."""
    num_updated: int = 0


# ── Search ───────────────────────────────────────────────────────────────

@dataclass
class Hit:
    """Single search result hit."""
    document: dict[str, Any] = field(default_factory=dict)
    text_match: int = 0
    highlights: list[dict[str, Any]] = field(default_factory=list)


@dataclass
class FacetCount:
    """Facet value with count."""
    value: str = ""
    count: int = 0


@dataclass
class FacetResult:
    """Facet field with value counts."""
    field_name: str = ""
    counts: list[FacetCount] = field(default_factory=list)


@dataclass
class SearchResult:
    """Search response from the server."""
    found: int = 0
    out_of: int = 0
    page: int = 1
    per_page: int = 10
    hits: list[Hit] = field(default_factory=list)
    facet_counts: list[FacetResult] = field(default_factory=list)
    search_time_ms: int = 0
    search_time_us: int = 0


@dataclass
class GroupedHit:
    """Group of search results."""
    group_key: list[str] = field(default_factory=list)
    hits: list[Hit] = field(default_factory=list)
    found: int = 0


@dataclass
class GroupedSearchResult:
    """Grouped search response."""
    found: int = 0
    out_of: int = 0
    grouped_hits: list[GroupedHit] = field(default_factory=list)
    search_time_us: int = 0


class SearchParameters(TypedDict, total=False):
    """Typed search parameters for Typesense v30.1 API.

    All fields are optional. Pass as keyword arguments to ``search()``
    or construct a dict and unpack it::

        params = SearchParameters(q="laptop", query_by="title,description")
        results = client.search("products", **params)

    See https://typesense.org/docs/30.1/api/search.html for full details.
    """

    # ── Query ─────────────────────────────────────────────────────────
    q: str
    query_by: str
    query_by_weights: str
    prefix: bool | str
    num_typos: int | str

    # ── Filtering & sorting ───────────────────────────────────────────
    filter_by: str
    sort_by: str

    # ── Faceting ──────────────────────────────────────────────────────
    facet_by: str
    max_facet_values: int
    facet_query: str
    facet_strategy: str
    facet_sample_percent: int
    facet_sample_threshold: int
    facet_return_parent: str

    # ── Grouping ──────────────────────────────────────────────────────
    group_by: str
    group_limit: int
    group_missing_values: bool

    # ── Pagination ────────────────────────────────────────────────────
    per_page: int
    page: int
    offset: int
    limit: int

    # ── Field selection ───────────────────────────────────────────────
    include_fields: str
    exclude_fields: str

    # ── Highlighting ──────────────────────────────────────────────────
    highlight_fields: str
    highlight_full_fields: str
    highlight_start_tag: str
    highlight_end_tag: str
    highlight_affix_num_tokens: int
    snippet_threshold: int

    # ── Pinning / hiding ──────────────────────────────────────────────
    pinned_hits: str
    hidden_hits: str
    enable_overrides: bool

    # ── Token dropping ────────────────────────────────────────────────
    drop_tokens_threshold: int
    drop_tokens_mode: str
    typo_tokens_threshold: int

    # ── Typo tolerance ────────────────────────────────────────────────
    min_len_1typo: int
    min_len_2typo: int
    split_join_tokens: str
    synonym_num_typos: int
    enable_typos_for_numerical_tokens: bool
    enable_typos_for_alpha_numerical_tokens: bool

    # ── Infix search ──────────────────────────────────────────────────
    infix: str
    max_extra_prefix: int
    max_extra_suffix: int

    # ── Performance ───────────────────────────────────────────────────
    exhaustive_search: bool
    search_cutoff_ms: int
    max_candidates: int

    # ── Caching ───────────────────────────────────────────────────────
    use_cache: bool
    cache_ttl: int

    # ── Validation & toggles ──────────────────────────────────────────
    validate_field_names: bool
    enable_synonyms: bool

    # ── Preset ────────────────────────────────────────────────────────
    preset: str

    # ── Stopwords & synonyms ──────────────────────────────────────────
    stopwords: str
    synonym_sets: str

    # ── Vector search ─────────────────────────────────────────────────
    vector_query: str

    # ── Decay sorting ─────────────────────────────────────────────────
    text_match_type: str

    # ── Priority (Torque extension) ───────────────────────────────────
    prioritize_exact_match: bool
    prioritize_token_position: bool
    prioritize_num_matching_fields: bool


# ── Aliases ──────────────────────────────────────────────────────────────

@dataclass
class CollectionAlias:
    """Collection alias."""
    name: str = ""
    collection_name: str = ""


# ── API Keys ─────────────────────────────────────────────────────────────

@dataclass
class ApiKey:
    """API key."""
    id: int = 0
    description: str = ""
    actions: list[str] = field(default_factory=list)
    collections: list[str] = field(default_factory=list)
    value: str | None = None
    expires_at: int | None = None


# ── Synonyms ─────────────────────────────────────────────────────────────

@dataclass
class SynonymItem:
    """Synonym rule (one-way or multi-way)."""
    id: str = ""
    synonyms: list[str] = field(default_factory=list)
    root: str | None = None
    locale: str | None = None


@dataclass
class SynonymSet:
    """Named synonym set containing items."""
    name: str = ""
    items: list[SynonymItem] = field(default_factory=list)


# ── Presets ──────────────────────────────────────────────────────────────

@dataclass
class Preset:
    """Saved search preset."""
    id: str = ""
    value: dict[str, Any] = field(default_factory=dict)


# ── Stopwords ────────────────────────────────────────────────────────────

@dataclass
class StopwordSet:
    """Named stopword set."""
    id: str = ""
    stopwords: list[str] = field(default_factory=list)


# ── Health ───────────────────────────────────────────────────────────────

@dataclass
class HealthResponse:
    """Server health response."""
    ok: bool = False
    version: str = ""


# ── Admin ────────────────────────────────────────────────────────────────

@dataclass
class AdminStats:
    """Server admin statistics."""
    system: dict[str, Any] = field(default_factory=dict)
    collections: list[dict[str, Any]] = field(default_factory=list)
