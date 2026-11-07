// Package torque provides an HTTP client for the Torque search engine.
// Typesense v30.1 compatible API. Zero external dependencies. Thread-safe.
//
// Usage:
//
//	// Single-node:
//	client := torque.NewHttpClient("http://localhost:8108", &torque.Options{ApiKey: "your-key"})
//	defer client.Close()
//	result, _ := client.Search(ctx, "products", map[string]string{"q": "laptop", "query_by": "title"})
//
//	// Multi-node with failover:
//	client := torque.NewHttpClient("", &torque.Options{
//	    ApiKey: "your-key",
//	    Nodes: []torque.NodeConfig{
//	        {Host: "node1.example.com", Port: 8108, Protocol: "https"},
//	        {Host: "node2.example.com", Port: 8108, Protocol: "https"},
//	    },
//	    NumRetries: 3,
//	})
package torque

import (
	"bytes"
	"context"
	"crypto/hmac"
	"crypto/sha256"
	"crypto/tls"
	"encoding/base64"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"os"
	"strconv"
	"strings"
	"sync/atomic"
	"time"
)

// ── Error Types ────────────────────────────────────────────────────────

// TorqueError is the base error type for all Torque API errors.
type TorqueError struct {
	Message    string
	StatusCode int
}

func (e *TorqueError) Error() string {
	if e.StatusCode > 0 {
		return fmt.Sprintf("torque: HTTP %d: %s", e.StatusCode, e.Message)
	}
	return fmt.Sprintf("torque: %s", e.Message)
}

// ObjectNotFoundError is returned when the requested resource does not exist (HTTP 404).
type ObjectNotFoundError struct {
	TorqueError
}

// UnauthorizedError is returned when authentication fails (HTTP 401).
type UnauthorizedError struct {
	TorqueError
}

// ForbiddenError is returned when the request is forbidden (HTTP 403).
type ForbiddenError struct {
	TorqueError
}

// ConflictError is returned when the request conflicts with existing state (HTTP 409).
type ConflictError struct {
	TorqueError
}

// BadRequestError is returned when the request is malformed (HTTP 400).
type BadRequestError struct {
	TorqueError
}

// ServerError is returned when the server encounters an internal error (HTTP 5xx).
type ServerError struct {
	TorqueError
}

// ServiceUnavailableError is returned when the server is temporarily unavailable (HTTP 503).
type ServiceUnavailableError struct {
	TorqueError
}

// RequestError is an alias for BadRequestError for backward compatibility.
type RequestError = BadRequestError

// newTypedError maps HTTP status codes to typed error values.
func newTypedError(statusCode int, message string) error {
	base := TorqueError{Message: message, StatusCode: statusCode}
	switch {
	case statusCode == 400:
		return &BadRequestError{base}
	case statusCode == 401:
		return &UnauthorizedError{base}
	case statusCode == 403:
		return &ForbiddenError{base}
	case statusCode == 404:
		return &ObjectNotFoundError{base}
	case statusCode == 409:
		return &ConflictError{base}
	case statusCode == 503:
		return &ServiceUnavailableError{base}
	case statusCode >= 500:
		return &ServerError{base}
	default:
		return &base
	}
}

// ── Node Health Tracking ───────────────────────────────────────────────

// NodeConfig describes a single Torque server node.
type NodeConfig struct {
	Host     string
	Port     int
	Protocol string // "http" or "https"
}

func (n NodeConfig) url() string {
	proto := n.Protocol
	if proto == "" {
		proto = "http"
	}
	if n.Port == 0 {
		return fmt.Sprintf("%s://%s", proto, n.Host)
	}
	return fmt.Sprintf("%s://%s:%d", proto, n.Host, n.Port)
}

type nodeState struct {
	config        NodeConfig
	baseURL       string
	isHealthy     atomic.Bool
	lastChecked   atomic.Int64 // unix millis
}

// ── Options ────────────────────────────────────────────────────────────

// Options configures the Torque client.
type Options struct {
	ApiKey                    string
	Timeout                   time.Duration // Per-request timeout. Default: 30s.
	TransferTimeout           time.Duration // Timeout for large transfers (import/export). Default: 5min.
	AllowUntrustedCertificate bool

	// SendApiKeyAsQueryParam sends the API key as ?x-typesense-api-key=KEY
	// instead of the X-TYPESENSE-API-KEY header. Useful for clients that
	// cannot set custom request headers (e.g. browser WebSocket connections).
	SendApiKeyAsQueryParam bool

	// Multi-node failover. If Nodes is non-empty, the baseURL parameter
	// to NewHttpClient is ignored and requests are distributed across nodes.
	Nodes []NodeConfig

	// NumRetries is the maximum number of retry attempts per request.
	// Default: 3. Set to 0 to disable retries.
	NumRetries int

	// RetryInterval is the initial delay between retries.
	// Subsequent retries use exponential backoff (2x each retry).
	// Default: 100ms.
	RetryInterval time.Duration

	// HealthcheckInterval controls how long a node stays marked unhealthy
	// before being retried. Default: 60s.
	HealthcheckInterval time.Duration
}

// ImportOptions configures document import behavior.
type ImportOptions struct {
	// Action controls how documents are written.
	// Valid values: "create", "upsert", "update", "emplace". Default: "create".
	Action string

	// BatchSize, when >0, chunks the JSONL into batches of this many lines.
	// The server processes each batch independently; results are concatenated.
	BatchSize int

	// ReturnDoc, when true, instructs the server to include the full document
	// in each per-line result object.
	ReturnDoc bool

	// ReturnId, when true, instructs the server to include the document ID
	// in each per-line result object.
	ReturnId bool

	// DirtyValues controls coercion behavior for fields that don't match the
	// schema type. Valid values: "coerce_or_reject", "coerce_or_drop", "drop",
	// "reject". Pass "" to use the server default.
	DirtyValues string
}

// ExportOptions configures document export behavior.
type ExportOptions struct {
	FilterBy      string
	IncludeFields string
	ExcludeFields string
}

// ── Interface ──────────────────────────────────────────────────────────

// TorqueHttpClient is the interface implemented by *HttpClient. Use it in
// application code to allow swapping the real client with a mock in tests.
type TorqueHttpClient interface {
	// Health / Stats
	Health(ctx context.Context) (map[string]any, error)
	Stats(ctx context.Context) (map[string]any, error)
	Metrics(ctx context.Context) (string, error)

	// Collections
	CreateCollection(ctx context.Context, schema map[string]any) (map[string]any, error)
	ListCollections(ctx context.Context) ([]map[string]any, error)
	GetCollection(ctx context.Context, name string) (map[string]any, error)
	UpdateCollection(ctx context.Context, name string, schema map[string]any) (map[string]any, error)
	DeleteCollection(ctx context.Context, name string) error

	// Documents
	CreateDocument(ctx context.Context, collection string, document map[string]any, dirtyValues ...string) (map[string]any, error)
	UpsertDocument(ctx context.Context, collection string, doc map[string]any, dirtyValues ...string) (map[string]any, error)
	GetDocument(ctx context.Context, collection, documentID string) (map[string]any, error)
	UpdateDocument(ctx context.Context, collection, documentID string, fields map[string]any) (map[string]any, error)
	DeleteDocument(ctx context.Context, collection, documentID string) error
	DeleteDocumentsByFilter(ctx context.Context, collection string, filterBy string) (int, error)
	TruncateCollection(ctx context.Context, collection string) error
	UpdateDocumentsByFilter(ctx context.Context, collection string, doc map[string]any, filterBy string) (int, error)
	ImportDocuments(ctx context.Context, collection, jsonl, action string) (string, error)
	ImportDocumentsReader(ctx context.Context, collection string, reader io.Reader, action string) (string, error)
	ImportDocumentsWithOptions(ctx context.Context, collection, jsonl string, opts ImportOptions) (string, error)
	ImportDocumentsReaderWithOptions(ctx context.Context, collection string, reader io.Reader, opts ImportOptions) (string, error)
	ExportDocuments(ctx context.Context, collection string, opts *ExportOptions) (io.ReadCloser, error)
	ExportDocumentsString(ctx context.Context, collection string, opts *ExportOptions) (string, error)
	ImportBinary(ctx context.Context, collection, filePath string) (map[string]any, error)

	// Search
	Search(ctx context.Context, collection string, params map[string]string) (map[string]any, error)
	MultiSearch(ctx context.Context, searches []map[string]any, commonParams map[string]string) (map[string]any, error)
	MultiSearchUnion(ctx context.Context, searches []map[string]any, commonParams map[string]string) (map[string]any, error)

	// Aliases
	UpsertAlias(ctx context.Context, name, collectionName string) (map[string]any, error)
	GetAlias(ctx context.Context, name string) (map[string]any, error)
	ListAliases(ctx context.Context) (map[string]any, error)
	DeleteAlias(ctx context.Context, name string) error

	// API Keys
	CreateKey(ctx context.Context, keySpec map[string]any) (map[string]any, error)
	ListKeys(ctx context.Context) (map[string]any, error)
	GetKey(ctx context.Context, id int) (map[string]any, error)
	DeleteKey(ctx context.Context, id int) error

	// Synonyms
	UpsertSynonymSet(ctx context.Context, name string, body map[string]any) (map[string]any, error)
	GetSynonymSet(ctx context.Context, collection, name string) (map[string]any, error)
	ListSynonymSets(ctx context.Context) (map[string]any, error)
	DeleteSynonymSet(ctx context.Context, name string) error

	// Presets
	UpsertPreset(ctx context.Context, id string, body map[string]any) (map[string]any, error)
	GetPreset(ctx context.Context, name string) (map[string]any, error)
	ListPresets(ctx context.Context) (map[string]any, error)
	DeletePreset(ctx context.Context, id string) error

	// Stopwords
	UpsertStopwords(ctx context.Context, id string, body map[string]any) (map[string]any, error)
	GetStopwordsSet(ctx context.Context, name string) (map[string]any, error)
	ListStopwords(ctx context.Context) (map[string]any, error)
	DeleteStopwords(ctx context.Context, id string) error

	// Lifecycle
	Close()
}

// ── Backward-compat aliases ────────────────────────────────────────────




// ── Client ─────────────────────────────────────────────────────────────

// HttpClient is the Torque HTTP client. Thread-safe for concurrent use.
type HttpClient struct {
	apiKey                 string
	sendApiKeyAsQueryParam bool
	http                   *http.Client
	timeout                time.Duration
	transferTimeout        time.Duration
	healthcheckInterval    time.Duration
	numRetries             int
	retryInterval          time.Duration

	// Single-node mode: nodes has exactly one entry.
	// Multi-node mode: round-robin across healthy nodes.
	nodes     []*nodeState
	nodeIndex atomic.Uint64
}

// NewHttpClient creates a new Torque HTTP client.
//
// For single-node usage, pass the base URL directly:
//
//	client := torque.NewHttpClient("http://localhost:8108", &torque.Options{ApiKey: "key"})
//
// For multi-node failover, use Options.Nodes and pass an empty baseURL:
//
//	client := torque.NewHttpClient("", &torque.Options{
//	    ApiKey: "key",
//	    Nodes:  []torque.NodeConfig{{Host: "n1", Port: 8108, Protocol: "https"}},
//	})
func NewHttpClient(baseURL string, opts *Options) *HttpClient {
	if opts == nil {
		opts = &Options{}
	}
	timeout := opts.Timeout
	if timeout == 0 {
		timeout = 30 * time.Second
	}
	transferTimeout := opts.TransferTimeout
	if transferTimeout == 0 {
		transferTimeout = 5 * time.Minute
	}
	numRetries := opts.NumRetries
	if numRetries == 0 && len(opts.Nodes) > 0 {
		numRetries = 3
	}
	retryInterval := opts.RetryInterval
	if retryInterval == 0 {
		retryInterval = 100 * time.Millisecond
	}
	healthcheckInterval := opts.HealthcheckInterval
	if healthcheckInterval == 0 {
		healthcheckInterval = 60 * time.Second
	}

	transport := http.DefaultTransport.(*http.Transport).Clone()
	if opts.AllowUntrustedCertificate {
		transport.TLSClientConfig = &tls.Config{InsecureSkipVerify: true}
	}

	// Build node list
	var nodes []*nodeState
	if len(opts.Nodes) > 0 {
		for _, nc := range opts.Nodes {
			ns := &nodeState{config: nc, baseURL: nc.url()}
			ns.isHealthy.Store(true)
			nodes = append(nodes, ns)
		}
	} else {
		ns := &nodeState{baseURL: strings.TrimRight(baseURL, "/")}
		ns.isHealthy.Store(true)
		nodes = append(nodes, ns)
	}

	return &HttpClient{
		apiKey:                 opts.ApiKey,
		sendApiKeyAsQueryParam: opts.SendApiKeyAsQueryParam,
		timeout:                timeout,
		transferTimeout:        transferTimeout,
		healthcheckInterval:    healthcheckInterval,
		numRetries:             numRetries,
		retryInterval:          retryInterval,
		nodes:                  nodes,
		http:                   &http.Client{Transport: transport},
	}
}

// Close releases idle connections.
func (c *HttpClient) Close() { c.http.CloseIdleConnections() }

// ── Node Selection ─────────────────────────────────────────────────────

// nextNode returns the next healthy node using round-robin.
// If all nodes are unhealthy and the healthcheck interval has elapsed
// for at least one, it is returned. Returns nil only if nodes is empty.
func (c *HttpClient) nextNode() *nodeState {
	n := len(c.nodes)
	if n == 0 {
		return nil
	}
	if n == 1 {
		return c.nodes[0]
	}

	start := c.nodeIndex.Add(1)
	nowMs := time.Now().UnixMilli()

	// First pass: find a healthy node
	for i := 0; i < n; i++ {
		node := c.nodes[(int(start)+i)%n]
		if node.isHealthy.Load() {
			return node
		}
	}

	// All unhealthy: find one whose healthcheck interval has elapsed
	for i := 0; i < n; i++ {
		node := c.nodes[(int(start)+i)%n]
		lastChecked := node.lastChecked.Load()
		if nowMs-lastChecked >= c.healthcheckInterval.Milliseconds() {
			node.lastChecked.Store(nowMs)
			return node
		}
	}

	// Fallback: return the next node regardless
	return c.nodes[int(start)%n]
}

func (c *HttpClient) markUnhealthy(node *nodeState) {
	node.isHealthy.Store(false)
	node.lastChecked.Store(time.Now().UnixMilli())
}

func (c *HttpClient) markHealthy(node *nodeState) {
	node.isHealthy.Store(true)
}

// ── Health ──────────────────────────────────────────────────────────────

// Health returns the server health status.
func (c *HttpClient) Health(ctx context.Context) (map[string]any, error) {
	return c.doGet(ctx, "/health", c.timeout)
}

// Stats returns server statistics including memory and index info.
func (c *HttpClient) Stats(ctx context.Context) (map[string]any, error) {
	return c.doGet(ctx, "/admin/stats", c.timeout)
}

// Metrics returns Prometheus-format metrics as text.
func (c *HttpClient) Metrics(ctx context.Context) (string, error) {
	return c.doGetText(ctx, "/metrics", c.timeout)
}

// ── Collections ─────────────────────────────────────────────────────────

// CreateCollection creates a new collection with the given schema.
func (c *HttpClient) CreateCollection(ctx context.Context, schema map[string]any) (map[string]any, error) {
	return c.doPost(ctx, "/collections", schema, c.timeout)
}

// ListCollections returns all collections.
func (c *HttpClient) ListCollections(ctx context.Context) ([]map[string]any, error) {
	return c.doGetList(ctx, "/collections", c.timeout)
}

// GetCollection returns a single collection by name.
func (c *HttpClient) GetCollection(ctx context.Context, name string) (map[string]any, error) {
	return c.doGet(ctx, "/collections/"+url.PathEscape(name), c.timeout)
}

// UpdateCollection updates a collection schema (PATCH).
func (c *HttpClient) UpdateCollection(ctx context.Context, name string, schema map[string]any) (map[string]any, error) {
	return c.doPatch(ctx, "/collections/"+url.PathEscape(name), schema, c.timeout)
}

// DeleteCollection deletes a collection by name.
func (c *HttpClient) DeleteCollection(ctx context.Context, name string) error {
	_, err := c.doDelete(ctx, "/collections/"+url.PathEscape(name), c.timeout)
	return err
}

// ── Documents ───────────────────────────────────────────────────────────

// CreateDocument creates a new document in the collection.
// dirtyValues controls coercion behavior for dirty field values; pass "" to
// omit. Valid values: "coerce_or_reject", "coerce_or_drop", "drop", "reject".
func (c *HttpClient) CreateDocument(ctx context.Context, collection string, document map[string]any, dirtyValues ...string) (map[string]any, error) {
	path := "/collections/" + url.PathEscape(collection) + "/documents"
	if len(dirtyValues) > 0 && dirtyValues[0] != "" {
		path += "?dirty_values=" + url.QueryEscape(dirtyValues[0])
	}
	return c.doPost(ctx, path, document, c.timeout)
}

// UpsertDocument creates or replaces a document in the collection.
// dirtyValues controls coercion behavior for dirty field values; pass "" to
// omit. Valid values: "coerce_or_reject", "coerce_or_drop", "drop", "reject".
func (c *HttpClient) UpsertDocument(ctx context.Context, collection string, doc map[string]any, dirtyValues ...string) (map[string]any, error) {
	path := "/collections/" + url.PathEscape(collection) + "/documents?action=upsert"
	if len(dirtyValues) > 0 && dirtyValues[0] != "" {
		path += "&dirty_values=" + url.QueryEscape(dirtyValues[0])
	}
	return c.doPost(ctx, path, doc, c.timeout)
}

// GetDocument retrieves a document by ID.
func (c *HttpClient) GetDocument(ctx context.Context, collection, documentID string) (map[string]any, error) {
	return c.doGet(ctx, "/collections/"+url.PathEscape(collection)+"/documents/"+url.PathEscape(documentID), c.timeout)
}

// UpdateDocument partially updates a document by ID (PATCH).
func (c *HttpClient) UpdateDocument(ctx context.Context, collection, documentID string, fields map[string]any) (map[string]any, error) {
	return c.doPatch(ctx, "/collections/"+url.PathEscape(collection)+"/documents/"+url.PathEscape(documentID), fields, c.timeout)
}

// DeleteDocument deletes a document by ID.
func (c *HttpClient) DeleteDocument(ctx context.Context, collection, documentID string) error {
	_, err := c.doDelete(ctx, "/collections/"+url.PathEscape(collection)+"/documents/"+url.PathEscape(documentID), c.timeout)
	return err
}

// DeleteDocumentsByFilter deletes documents matching a filter expression.
// Returns the number of documents deleted.
func (c *HttpClient) DeleteDocumentsByFilter(ctx context.Context, collection string, filterBy string) (int, error) {
	path := "/collections/" + url.PathEscape(collection) + "/documents?filter_by=" + url.QueryEscape(filterBy)
	result, err := c.doDeleteJSON(ctx, path, c.timeout)
	if err != nil {
		return 0, err
	}
	return extractNumDeleted(result), nil
}

// TruncateCollection removes all documents from the collection while keeping
// its schema and configuration. Equivalent to DELETE /collections/{name}/documents
// with no filter_by parameter.
func (c *HttpClient) TruncateCollection(ctx context.Context, collection string) error {
	_, err := c.doDeleteJSON(ctx, "/collections/"+url.PathEscape(collection)+"/documents", c.timeout)
	return err
}

// UpdateDocumentsByFilter partially updates documents matching a filter expression (PATCH).
// Returns the number of documents updated.
func (c *HttpClient) UpdateDocumentsByFilter(ctx context.Context, collection string, doc map[string]any, filterBy string) (int, error) {
	path := "/collections/" + url.PathEscape(collection) + "/documents?filter_by=" + url.QueryEscape(filterBy)
	result, err := c.doPatch(ctx, path, doc, c.timeout)
	if err != nil {
		return 0, err
	}
	return extractNumUpdated(result), nil
}

// ImportDocuments imports via JSONL string. action can be "create", "upsert",
// "update", or "emplace". Returns per-line results as text.
func (c *HttpClient) ImportDocuments(ctx context.Context, collection, jsonl, action string) (string, error) {
	return c.doPostRaw(
		ctx,
		"/collections/"+url.PathEscape(collection)+"/documents/import?action="+url.QueryEscape(action),
		jsonl, "text/plain", c.transferTimeout,
	)
}

// ImportDocumentsReader streams documents from an io.Reader to the import
// endpoint. The reader should produce JSONL (one JSON document per line).
// action can be "create", "upsert", "update", or "emplace".
// Returns per-line results as text.
func (c *HttpClient) ImportDocumentsReader(ctx context.Context, collection string, reader io.Reader, action string) (string, error) {
	return c.doPostReader(
		ctx,
		"/collections/"+url.PathEscape(collection)+"/documents/import?action="+url.QueryEscape(action),
		reader, "text/plain", c.transferTimeout,
	)
}

// ImportDocumentsWithOptions imports documents from a JSONL string using the
// provided ImportOptions. Returns per-line results as text.
func (c *HttpClient) ImportDocumentsWithOptions(ctx context.Context, collection, jsonl string, opts ImportOptions) (string, error) {
	path := c.buildImportPath(collection, opts)
	return c.doPostRaw(ctx, path, jsonl, "text/plain", c.transferTimeout)
}

// ImportDocumentsReaderWithOptions streams documents from an io.Reader using
// the provided ImportOptions. Returns per-line results as text.
func (c *HttpClient) ImportDocumentsReaderWithOptions(ctx context.Context, collection string, reader io.Reader, opts ImportOptions) (string, error) {
	path := c.buildImportPath(collection, opts)
	return c.doPostReader(ctx, path, reader, "text/plain", c.transferTimeout)
}

// buildImportPath constructs the import endpoint URL from ImportOptions.
func (c *HttpClient) buildImportPath(collection string, opts ImportOptions) string {
	action := opts.Action
	if action == "" {
		action = "create"
	}
	q := url.Values{}
	q.Set("action", action)
	if opts.BatchSize > 0 {
		q.Set("batch_size", strconv.Itoa(opts.BatchSize))
	}
	if opts.ReturnDoc {
		q.Set("return_doc", "true")
	}
	if opts.ReturnId {
		q.Set("return_id", "true")
	}
	if opts.DirtyValues != "" {
		q.Set("dirty_values", opts.DirtyValues)
	}
	return "/collections/" + url.PathEscape(collection) + "/documents/import?" + q.Encode()
}

// ExportDocuments returns a streaming reader of JSONL documents with optional filtering.
// The caller must close the returned io.ReadCloser when done.
func (c *HttpClient) ExportDocuments(ctx context.Context, collection string, opts *ExportOptions) (io.ReadCloser, error) {
	path := "/collections/" + url.PathEscape(collection) + "/documents/export"
	if opts != nil {
		q := url.Values{}
		if opts.FilterBy != "" {
			q.Set("filter_by", opts.FilterBy)
		}
		if opts.IncludeFields != "" {
			q.Set("include_fields", opts.IncludeFields)
		}
		if opts.ExcludeFields != "" {
			q.Set("exclude_fields", opts.ExcludeFields)
		}
		if encoded := q.Encode(); encoded != "" {
			path += "?" + encoded
		}
	}
	return c.doGetStream(ctx, path, c.transferTimeout)
}

// ExportDocumentsString is a convenience wrapper around ExportDocuments that
// buffers the entire response into a string. For large collections, prefer
// ExportDocuments which returns a streaming io.ReadCloser.
func (c *HttpClient) ExportDocumentsString(ctx context.Context, collection string, opts *ExportOptions) (string, error) {
	rc, err := c.ExportDocuments(ctx, collection, opts)
	if err != nil {
		return "", err
	}
	defer rc.Close()
	data, err := io.ReadAll(rc)
	if err != nil {
		return "", fmt.Errorf("torque: read export response: %w", err)
	}
	return string(data), nil
}

// ImportBinary uploads a TQBF binary file for import.
func (c *HttpClient) ImportBinary(ctx context.Context, collection, filePath string) (map[string]any, error) {
	data, err := os.ReadFile(filePath)
	if err != nil {
		return nil, fmt.Errorf("torque: read binary file: %w", err)
	}
	return c.doPostBytes(ctx, "/collections/"+url.PathEscape(collection)+"/documents/import-binary", data)
}

// ── Search ──────────────────────────────────────────────────────────────

// Search executes a search query. Params are Typesense v30.1 search parameters.
func (c *HttpClient) Search(ctx context.Context, collection string, params map[string]string) (map[string]any, error) {
	q := url.Values{}
	for k, v := range params {
		q.Set(k, v)
	}
	return c.doGet(ctx, "/collections/"+url.PathEscape(collection)+"/documents/search?"+q.Encode(), c.timeout)
}

// MultiSearch executes multiple searches in one request. commonParams is an
// optional map of query parameters shared across all searches (e.g.
// "query_by"); pass nil if not needed.
func (c *HttpClient) MultiSearch(ctx context.Context, searches []map[string]any, commonParams map[string]string) (map[string]any, error) {
	path := "/multi_search"
	if len(commonParams) > 0 {
		q := url.Values{}
		for k, v := range commonParams {
			q.Set(k, v)
		}
		path += "?" + q.Encode()
	}
	return c.doPost(ctx, path, map[string]any{"searches": searches}, c.timeout)
}

// MultiSearchUnion executes multiple searches merged into a single ranked
// result set (Typesense v30.1 union: true). Hits from all searches are merged
// and ranked by text_match; page/per_page in commonParams paginate the merged
// set. Not compatible with group_by searches.
func (c *HttpClient) MultiSearchUnion(ctx context.Context, searches []map[string]any, commonParams map[string]string) (map[string]any, error) {
	path := "/multi_search"
	if len(commonParams) > 0 {
		q := url.Values{}
		for k, v := range commonParams {
			q.Set(k, v)
		}
		path += "?" + q.Encode()
	}
	return c.doPost(ctx, path, map[string]any{"searches": searches, "union": true}, c.timeout)
}

// ── Aliases ─────────────────────────────────────────────────────────────

// UpsertAlias creates or updates a collection alias.
func (c *HttpClient) UpsertAlias(ctx context.Context, name, collectionName string) (map[string]any, error) {
	return c.doPut(ctx, "/aliases/"+url.PathEscape(name), map[string]any{"collection_name": collectionName}, c.timeout)
}

// GetAlias returns a single alias by name.
func (c *HttpClient) GetAlias(ctx context.Context, name string) (map[string]any, error) {
	return c.doGet(ctx, "/aliases/"+url.PathEscape(name), c.timeout)
}

// ListAliases returns all aliases.
func (c *HttpClient) ListAliases(ctx context.Context) (map[string]any, error) {
	return c.doGet(ctx, "/aliases", c.timeout)
}

// DeleteAlias deletes an alias by name.
func (c *HttpClient) DeleteAlias(ctx context.Context, name string) error {
	_, err := c.doDelete(ctx, "/aliases/"+url.PathEscape(name), c.timeout)
	return err
}

// ── API Keys ────────────────────────────────────────────────────────────

// CreateKey creates a new API key.
func (c *HttpClient) CreateKey(ctx context.Context, keySpec map[string]any) (map[string]any, error) {
	return c.doPost(ctx, "/keys", keySpec, c.timeout)
}

// ListKeys returns all API keys.
func (c *HttpClient) ListKeys(ctx context.Context) (map[string]any, error) {
	return c.doGet(ctx, "/keys", c.timeout)
}

// GetKey returns an API key by ID.
func (c *HttpClient) GetKey(ctx context.Context, id int) (map[string]any, error) {
	return c.doGet(ctx, fmt.Sprintf("/keys/%d", id), c.timeout)
}

// DeleteKey deletes an API key by ID.
func (c *HttpClient) DeleteKey(ctx context.Context, id int) error {
	_, err := c.doDelete(ctx, fmt.Sprintf("/keys/%d", id), c.timeout)
	return err
}

// ── Synonyms ────────────────────────────────────────────────────────────

// UpsertSynonymSet creates or updates a synonym set.
func (c *HttpClient) UpsertSynonymSet(ctx context.Context, name string, body map[string]any) (map[string]any, error) {
	return c.doPut(ctx, "/synonym_sets/"+url.PathEscape(name), body, c.timeout)
}

// GetSynonymSet returns a single synonym set by name within a collection.
func (c *HttpClient) GetSynonymSet(ctx context.Context, collection, name string) (map[string]any, error) {
	return c.doGet(ctx, "/collections/"+url.PathEscape(collection)+"/synonym_sets/"+url.PathEscape(name), c.timeout)
}

// ListSynonymSets returns all synonym sets.
func (c *HttpClient) ListSynonymSets(ctx context.Context) (map[string]any, error) {
	return c.doGet(ctx, "/synonym_sets", c.timeout)
}

// DeleteSynonymSet deletes a synonym set by name.
func (c *HttpClient) DeleteSynonymSet(ctx context.Context, name string) error {
	_, err := c.doDelete(ctx, "/synonym_sets/"+url.PathEscape(name), c.timeout)
	return err
}

// ── Presets ──────────────────────────────────────────────────────────────

// UpsertPreset creates or updates a preset.
func (c *HttpClient) UpsertPreset(ctx context.Context, id string, body map[string]any) (map[string]any, error) {
	return c.doPut(ctx, "/presets/"+url.PathEscape(id), body, c.timeout)
}

// GetPreset returns a single preset by name.
func (c *HttpClient) GetPreset(ctx context.Context, name string) (map[string]any, error) {
	return c.doGet(ctx, "/presets/"+url.PathEscape(name), c.timeout)
}

// ListPresets returns all presets.
func (c *HttpClient) ListPresets(ctx context.Context) (map[string]any, error) {
	return c.doGet(ctx, "/presets", c.timeout)
}

// DeletePreset deletes a preset by ID.
func (c *HttpClient) DeletePreset(ctx context.Context, id string) error {
	_, err := c.doDelete(ctx, "/presets/"+url.PathEscape(id), c.timeout)
	return err
}

// ── Stopwords ───────────────────────────────────────────────────────────

// UpsertStopwords creates or updates a stopword set.
func (c *HttpClient) UpsertStopwords(ctx context.Context, id string, body map[string]any) (map[string]any, error) {
	return c.doPut(ctx, "/stopwords/"+url.PathEscape(id), body, c.timeout)
}

// GetStopwordsSet returns a single stopword set by name.
func (c *HttpClient) GetStopwordsSet(ctx context.Context, name string) (map[string]any, error) {
	return c.doGet(ctx, "/stopwords/"+url.PathEscape(name), c.timeout)
}

// ListStopwords returns all stopword sets.
func (c *HttpClient) ListStopwords(ctx context.Context) (map[string]any, error) {
	return c.doGet(ctx, "/stopwords", c.timeout)
}

// DeleteStopwords deletes a stopword set by ID.
func (c *HttpClient) DeleteStopwords(ctx context.Context, id string) error {
	_, err := c.doDelete(ctx, "/stopwords/"+url.PathEscape(id), c.timeout)
	return err
}

// ── Internal HTTP with Retry + Failover ────────────────────────────────

// applyApiKey appends the API key as a query parameter when
// SendApiKeyAsQueryParam is enabled; otherwise returns path unchanged.
func (c *HttpClient) applyApiKey(path string) string {
	if !c.sendApiKeyAsQueryParam {
		return path
	}
	sep := "?"
	if strings.Contains(path, "?") {
		sep = "&"
	}
	return path + sep + "x-typesense-api-key=" + url.QueryEscape(c.apiKey)
}

// doRequest executes an HTTP request with retry and multi-node failover.
// It uses context.WithTimeout instead of mutating http.Client.Timeout.
func (c *HttpClient) doRequest(ctx context.Context, method, path string, body io.Reader, contentType string, timeout time.Duration) (*http.Response, []byte, error) {
	maxAttempts := c.numRetries + 1
	if maxAttempts < 1 {
		maxAttempts = 1
	}

	var lastErr error
	for attempt := 0; attempt < maxAttempts; attempt++ {
		node := c.nextNode()
		if node == nil {
			return nil, nil, &TorqueError{Message: "no nodes configured"}
		}

		reqCtx, cancel := context.WithTimeout(ctx, timeout)
		reqPath := c.applyApiKey(path)
		req, err := http.NewRequestWithContext(reqCtx, method, node.baseURL+reqPath, body)
		if err != nil {
			cancel()
			return nil, nil, fmt.Errorf("torque: create request: %w", err)
		}
		if !c.sendApiKeyAsQueryParam {
			req.Header.Set("X-TYPESENSE-API-KEY", c.apiKey)
		}
		if contentType != "" {
			req.Header.Set("Content-Type", contentType)
		}

		resp, err := c.http.Do(req)
		if err != nil {
			cancel()
			c.markUnhealthy(node)
			lastErr = fmt.Errorf("torque: %s %s: %w", method, path, err)

			// If the caller's context is already done, don't retry
			if ctx.Err() != nil {
				return nil, nil, lastErr
			}

			// Exponential backoff before retry
			if attempt < maxAttempts-1 {
				delay := c.retryInterval * (1 << uint(attempt))
				timer := time.NewTimer(delay)
				select {
				case <-ctx.Done():
					timer.Stop()
					return nil, nil, lastErr
				case <-timer.C:
				}
			}
			continue
		}

		respBody, readErr := io.ReadAll(resp.Body)
		resp.Body.Close()
		cancel()

		if readErr != nil {
			c.markUnhealthy(node)
			lastErr = fmt.Errorf("torque: read response body: %w", readErr)
			if attempt < maxAttempts-1 {
				delay := c.retryInterval * (1 << uint(attempt))
				timer := time.NewTimer(delay)
				select {
				case <-ctx.Done():
					timer.Stop()
					return nil, nil, lastErr
				case <-timer.C:
				}
			}
			continue
		}

		c.markHealthy(node)

		// Retry on 503 Service Unavailable
		if resp.StatusCode == 503 && attempt < maxAttempts-1 {
			lastErr = newTypedError(resp.StatusCode, extractMessage(resp.StatusCode, respBody))
			delay := c.retryInterval * (1 << uint(attempt))
			timer := time.NewTimer(delay)
			select {
			case <-ctx.Done():
				timer.Stop()
				return nil, nil, lastErr
			case <-timer.C:
			}
			continue
		}

		return resp, respBody, nil
	}

	return nil, nil, lastErr
}

func (c *HttpClient) doGet(ctx context.Context, path string, timeout time.Duration) (map[string]any, error) {
	resp, body, err := c.doRequest(ctx, "GET", path, nil, "", timeout)
	if err != nil {
		return nil, err
	}
	if err := checkStatus(resp.StatusCode, body); err != nil {
		return nil, err
	}
	var result map[string]any
	if err := json.Unmarshal(body, &result); err != nil {
		return nil, fmt.Errorf("torque: unmarshal response: %w", err)
	}
	return result, nil
}

func (c *HttpClient) doGetList(ctx context.Context, path string, timeout time.Duration) ([]map[string]any, error) {
	resp, body, err := c.doRequest(ctx, "GET", path, nil, "", timeout)
	if err != nil {
		return nil, err
	}
	if err := checkStatus(resp.StatusCode, body); err != nil {
		return nil, err
	}
	var result []map[string]any
	if err := json.Unmarshal(body, &result); err != nil {
		return nil, fmt.Errorf("torque: unmarshal response: %w", err)
	}
	return result, nil
}

// doGetStream returns a streaming response body. The caller owns the
// returned ReadCloser and must close it. Unlike doRequest, the body is NOT
// buffered into memory.
func (c *HttpClient) doGetStream(ctx context.Context, path string, timeout time.Duration) (io.ReadCloser, error) {
	maxAttempts := c.numRetries + 1
	if maxAttempts < 1 {
		maxAttempts = 1
	}

	var lastErr error
	for attempt := 0; attempt < maxAttempts; attempt++ {
		node := c.nextNode()
		if node == nil {
			return nil, &TorqueError{Message: "no nodes configured"}
		}

		reqCtx, cancel := context.WithTimeout(ctx, timeout)
		reqPath := c.applyApiKey(path)
		req, err := http.NewRequestWithContext(reqCtx, "GET", node.baseURL+reqPath, nil)
		if err != nil {
			cancel()
			return nil, fmt.Errorf("torque: create request: %w", err)
		}
		if !c.sendApiKeyAsQueryParam {
			req.Header.Set("X-TYPESENSE-API-KEY", c.apiKey)
		}

		resp, err := c.http.Do(req)
		if err != nil {
			cancel()
			c.markUnhealthy(node)
			lastErr = fmt.Errorf("torque: GET %s: %w", path, err)
			if ctx.Err() != nil {
				return nil, lastErr
			}
			if attempt < maxAttempts-1 {
				delay := c.retryInterval * (1 << uint(attempt))
				timer := time.NewTimer(delay)
				select {
				case <-ctx.Done():
					timer.Stop()
					return nil, lastErr
				case <-timer.C:
				}
			}
			continue
		}

		c.markHealthy(node)

		if resp.StatusCode < 200 || resp.StatusCode >= 300 {
			body, _ := io.ReadAll(resp.Body)
			resp.Body.Close()
			cancel()
			return nil, newTypedError(resp.StatusCode, extractMessage(resp.StatusCode, body))
		}

		// Wrap body to cancel the timeout context when closed
		return &streamBody{ReadCloser: resp.Body, cancel: cancel}, nil
	}

	return nil, lastErr
}

// streamBody wraps a response body and cancels the request context on Close.
type streamBody struct {
	io.ReadCloser
	cancel context.CancelFunc
}

func (s *streamBody) Close() error {
	err := s.ReadCloser.Close()
	s.cancel()
	return err
}

func (c *HttpClient) doGetText(ctx context.Context, path string, timeout time.Duration) (string, error) {
	resp, body, err := c.doRequest(ctx, "GET", path, nil, "", timeout)
	if err != nil {
		return "", err
	}
	if err := checkStatus(resp.StatusCode, body); err != nil {
		return "", err
	}
	return string(body), nil
}

func (c *HttpClient) doPost(ctx context.Context, path string, payload any, timeout time.Duration) (map[string]any, error) {
	data, err := json.Marshal(payload)
	if err != nil {
		return nil, fmt.Errorf("torque: marshal request: %w", err)
	}
	resp, body, err := c.doRequest(ctx, "POST", path, bytes.NewReader(data), "application/json", timeout)
	if err != nil {
		return nil, err
	}
	if err := checkStatus(resp.StatusCode, body); err != nil {
		return nil, err
	}
	var result map[string]any
	if len(body) > 0 {
		if err := json.Unmarshal(body, &result); err != nil {
			return nil, fmt.Errorf("torque: unmarshal response: %w", err)
		}
	}
	return result, nil
}

func (c *HttpClient) doPostRaw(ctx context.Context, path, payload, contentType string, timeout time.Duration) (string, error) {
	resp, body, err := c.doRequest(ctx, "POST", path, strings.NewReader(payload), contentType, timeout)
	if err != nil {
		return "", err
	}
	if err := checkStatus(resp.StatusCode, body); err != nil {
		return "", err
	}
	return string(body), nil
}

func (c *HttpClient) doPostReader(ctx context.Context, path string, reader io.Reader, contentType string, timeout time.Duration) (string, error) {
	resp, body, err := c.doRequest(ctx, "POST", path, reader, contentType, timeout)
	if err != nil {
		return "", err
	}
	if err := checkStatus(resp.StatusCode, body); err != nil {
		return "", err
	}
	return string(body), nil
}

func (c *HttpClient) doPostBytes(ctx context.Context, path string, payload []byte) (map[string]any, error) {
	resp, body, err := c.doRequest(ctx, "POST", path, bytes.NewReader(payload), "application/octet-stream", c.transferTimeout)
	if err != nil {
		return nil, err
	}
	if err := checkStatus(resp.StatusCode, body); err != nil {
		return nil, err
	}
	var result map[string]any
	if err := json.Unmarshal(body, &result); err != nil {
		return nil, fmt.Errorf("torque: unmarshal response: %w", err)
	}
	return result, nil
}

func (c *HttpClient) doPut(ctx context.Context, path string, payload any, timeout time.Duration) (map[string]any, error) {
	data, err := json.Marshal(payload)
	if err != nil {
		return nil, fmt.Errorf("torque: marshal request: %w", err)
	}
	resp, body, err := c.doRequest(ctx, "PUT", path, bytes.NewReader(data), "application/json", timeout)
	if err != nil {
		return nil, err
	}
	if err := checkStatus(resp.StatusCode, body); err != nil {
		return nil, err
	}
	var result map[string]any
	if len(body) > 0 {
		if err := json.Unmarshal(body, &result); err != nil {
			return nil, fmt.Errorf("torque: unmarshal response: %w", err)
		}
	}
	return result, nil
}

func (c *HttpClient) doPatch(ctx context.Context, path string, payload any, timeout time.Duration) (map[string]any, error) {
	data, err := json.Marshal(payload)
	if err != nil {
		return nil, fmt.Errorf("torque: marshal request: %w", err)
	}
	resp, body, err := c.doRequest(ctx, "PATCH", path, bytes.NewReader(data), "application/json", timeout)
	if err != nil {
		return nil, err
	}
	if err := checkStatus(resp.StatusCode, body); err != nil {
		return nil, err
	}
	var result map[string]any
	if len(body) > 0 {
		if err := json.Unmarshal(body, &result); err != nil {
			return nil, fmt.Errorf("torque: unmarshal response: %w", err)
		}
	}
	return result, nil
}

func (c *HttpClient) doDelete(ctx context.Context, path string, timeout time.Duration) (string, error) {
	resp, body, err := c.doRequest(ctx, "DELETE", path, nil, "", timeout)
	if err != nil {
		return "", err
	}
	if err := checkStatus(resp.StatusCode, body); err != nil {
		return "", err
	}
	return string(body), nil
}

func (c *HttpClient) doDeleteJSON(ctx context.Context, path string, timeout time.Duration) (map[string]any, error) {
	resp, body, err := c.doRequest(ctx, "DELETE", path, nil, "", timeout)
	if err != nil {
		return nil, err
	}
	if err := checkStatus(resp.StatusCode, body); err != nil {
		return nil, err
	}
	var result map[string]any
	if err := json.Unmarshal(body, &result); err != nil {
		return nil, fmt.Errorf("torque: unmarshal response: %w", err)
	}
	return result, nil
}

// ── Status Checking ────────────────────────────────────────────────────

func checkStatus(code int, body []byte) error {
	if code >= 200 && code < 300 {
		return nil
	}
	return newTypedError(code, extractMessage(code, body))
}

// ── Scoped Search Keys ─────────────────────────────────────────────────

// GenerateScopedSearchKey generates an HMAC-SHA256 scoped search key that
// embeds the given params (e.g. "filter_by", "expires_at") into the key.
// searchKey is the parent search-only API key. The returned string is
// base64-encoded and safe to expose to end users.
func GenerateScopedSearchKey(searchKey string, params map[string]string) (string, error) {
	paramsJSON, err := json.Marshal(params)
	if err != nil {
		return "", fmt.Errorf("torque: marshal scoped key params: %w", err)
	}

	mac := hmac.New(sha256.New, []byte(searchKey))
	mac.Write(paramsJSON)
	digest := mac.Sum(nil)

	digestB64 := base64.StdEncoding.EncodeToString(digest)
	paramsB64 := base64.StdEncoding.EncodeToString(paramsJSON)
	keyB64 := base64.StdEncoding.EncodeToString([]byte(searchKey))

	return digestB64 + paramsB64 + keyB64, nil
}

// ── Helpers ────────────────────────────────────────────────────────────

// extractNumDeleted extracts the "num_deleted" count from an API response.
func extractNumDeleted(m map[string]any) int {
	if v, ok := m["num_deleted"]; ok {
		return toInt(v)
	}
	return 0
}

// extractNumUpdated extracts the "num_updated" count from an API response.
func extractNumUpdated(m map[string]any) int {
	if v, ok := m["num_updated"]; ok {
		return toInt(v)
	}
	return 0
}

// toInt converts a JSON number (float64) or string to int.
func toInt(v any) int {
	switch n := v.(type) {
	case float64:
		return int(n)
	case int:
		return n
	case string:
		i, _ := strconv.Atoi(n)
		return i
	default:
		return 0
	}
}

func extractMessage(code int, body []byte) string {
	var data map[string]any
	if json.Unmarshal(body, &data) == nil {
		if m, ok := data["message"].(string); ok {
			return m
		}
		if m, ok := data["error"].(string); ok {
			return m
		}
	}
	maxLen := len(body)
	if maxLen > 200 {
		maxLen = 200
	}
	return fmt.Sprintf("HTTP %d: %s", code, string(body[:maxLen]))
}
