package torque

import (
	"compress/zlib"
	"encoding/binary"
	"errors"
	"fmt"
	"io"
	"math"
	"net"
	"os"
)

// ── Protocol Constants ──────────────────────────────────────────────────

const (
	msgStartIngest = 0x01
	msgDocBatch    = 0x02
	msgCommit      = 0x03

	msgAck           = 0x10
	msgCommitResult  = 0x11
	msgError         = 0x12
	msgBackpressure  = 0x13

	tqbfVersion = 2
	flagZlib    = 2
)

// ── Language Codes ──────────────────────────────────────────────────────

// Language wire bytes matching the Rust Language enum.
const (
	LangEnglish    = 0
	LangSwedish    = 1
	LangNorwegian  = 2
	LangDanish     = 3
	LangFinnish    = 4
	LangGerman     = 5
	LangFrench     = 6
	LangSpanish    = 7
	LangPortuguese = 8
	LangItalian    = 9
	LangDutch      = 10
	LangRussian    = 11
	LangArabic     = 12
	LangGreek      = 13
	LangHungarian  = 14
	LangRomanian   = 15
	LangTurkish    = 16
)

// LangMap maps ISO 639-1 language codes to wire bytes.
var LangMap = map[string]byte{
	"en": LangEnglish,
	"sv": LangSwedish,
	"no": LangNorwegian,
	"da": LangDanish,
	"fi": LangFinnish,
	"de": LangGerman,
	"fr": LangFrench,
	"es": LangSpanish,
	"pt": LangPortuguese,
	"it": LangItalian,
	"nl": LangDutch,
	"ru": LangRussian,
	"ar": LangArabic,
	"el": LangGreek,
	"hu": LangHungarian,
	"ro": LangRomanian,
	"tr": LangTurkish,
}

// ── FieldDef ────────────────────────────────────────────────────────────

// FieldDef describes a single field in the collection schema.
type FieldDef struct {
	Name string
	Type string // "string", "int32", "int64", "float", "bool", "geopoint", "string[]", "int32[]", "int64[]", "float[]"
}

// GeoPoint represents a latitude/longitude pair.
type GeoPoint struct {
	Lat float64
	Lng float64
}

// ── BatchAck ────────────────────────────────────────────────────────────

// BatchAck is the server acknowledgement for a batch or commit.
type BatchAck struct {
	BatchSeq     uint64
	DocsReceived uint64
}

// CommitResult is the server response after a commit.
type CommitResult struct {
	Success    bool
	DocsIndexed uint64
	BuildMs     uint64
}

// ProtocolError is returned when the server sends an error message.
type ProtocolError struct {
	Code    uint16
	Message string
}

func (e *ProtocolError) Error() string {
	return fmt.Sprintf("torque protocol error %d: %s", e.Code, e.Message)
}

// ── Document Flattener ─────────────────────────────────────────────────


// flattenDocument flattens nested objects/arrays to dotted keys matching
// the Rust server's flatten_json_object logic.
func flattenDocument(doc map[string]any) map[string]any {
	return flattenInner(doc, 0)
}

func flattenInner(obj map[string]any, depth int) map[string]any {

	result := make(map[string]any, len(obj))
	for key, value := range obj {
		switch v := value.(type) {
		case map[string]any:
			// Nested object: flatten with dotted prefix
			flatInner := flattenInner(v, depth+1)
			for innerKey, innerVal := range flatInner {
				result[key+"."+innerKey] = innerVal
			}
		case []any:
			if len(v) > 0 {
				if _, isObj := v[0].(map[string]any); isObj {
					// Array of objects: pivot to object of arrays
					seen := make(map[string]bool)
					var allFieldNames []string
					for _, item := range v {
						if itemObj, ok := item.(map[string]any); ok {
							for fieldName := range itemObj {
								if !seen[fieldName] {
									seen[fieldName] = true
									allFieldNames = append(allFieldNames, fieldName)
								}
							}
						}
					}
					for _, fieldName := range allFieldNames {
						arrKey := key + "." + fieldName
						values := make([]any, 0, len(v))
						for _, item := range v {
							if itemObj, ok := item.(map[string]any); ok {
								values = append(values, itemObj[fieldName])
							} else {
								values = append(values, nil)
							}
						}
						result[arrKey] = values
					}
				} else {
					result[key] = value
				}
			} else {
				result[key] = value
			}
		default:
			result[key] = value
		}
	}
	return result
}

// ── DocumentEncoder ─────────────────────────────────────────────────────

// DocumentEncoder encodes document maps to binary wire format.
type DocumentEncoder struct{}

// Encode encodes a single document into binary format.
func (DocumentEncoder) Encode(doc map[string]any, schema []FieldDef) ([]byte, error) {
	doc = flattenDocument(doc)
	size := computeDocSize(doc, schema)
	buf := make([]byte, size)
	pos := 0

	// External ID (u16 BE length + UTF-8)
	id := toString(doc["id"])
	idBytes := []byte(id)
	binary.BigEndian.PutUint16(buf[pos:], uint16(len(idBytes)))
	pos += 2
	copy(buf[pos:], idBytes)
	pos += len(idBytes)

	// Language byte
	lang := byte(0)
	if v, ok := doc["_language"]; ok {
		if l, ok := v.(int); ok {
			lang = byte(l)
		}
	}
	buf[pos] = lang
	pos++

	// Field count (u16 BE)
	binary.BigEndian.PutUint16(buf[pos:], uint16(len(schema)))
	pos += 2

	// Fields in schema order
	for _, field := range schema {
		value, exists := doc[field.Name]
		if !exists || value == nil {
			buf[pos] = 0 // null
			pos++
		} else {
			buf[pos] = 1 // present
			pos++
			var err error
			pos, err = writeFieldValue(buf, pos, value, field.Type)
			if err != nil {
				return nil, fmt.Errorf("field %q: %w", field.Name, err)
			}
		}
	}

	return buf[:pos], nil
}

func computeDocSize(doc map[string]any, schema []FieldDef) int {
	id := toString(doc["id"])
	// u16 id_len + id_bytes + u8 language + u16 field_count
	size := 2 + len(id) + 1 + 2

	for _, field := range schema {
		size++ // presence byte
		value, exists := doc[field.Name]
		if exists && value != nil {
			size += computeFieldSize(value, field.Type)
		}
	}
	return size
}

func computeFieldSize(value any, typ string) int {
	switch typ {
	case "string":
		return 4 + len(toString(value))
	case "int32":
		return 4
	case "int64":
		return 8
	case "float":
		return 8
	case "bool":
		return 1
	case "geopoint":
		return 16
	case "string[]":
		arr := toAnySlice(value)
		s := 2 // u16 array length
		for _, el := range arr {
			s++ // null flag
			if el != nil {
				s += 4 + len(toString(el))
			}
		}
		return s
	case "int32[]":
		arr := toAnySlice(value)
		s := 2
		for _, el := range arr {
			s++
			if el != nil {
				s += 4
			}
		}
		return s
	case "int64[]":
		arr := toAnySlice(value)
		s := 2
		for _, el := range arr {
			s++
			if el != nil {
				s += 8
			}
		}
		return s
	case "float[]":
		arr := toAnySlice(value)
		return 2 + len(arr)*4
	default:
		return 0
	}
}

func writeFieldValue(buf []byte, pos int, value any, typ string) (int, error) {
	switch typ {
	case "string":
		s := toString(value)
		sBytes := []byte(s)
		binary.BigEndian.PutUint32(buf[pos:], uint32(len(sBytes)))
		pos += 4
		copy(buf[pos:], sBytes)
		pos += len(sBytes)
		return pos, nil

	case "int32":
		v, err := toInt32(value)
		if err != nil {
			return pos, err
		}
		binary.BigEndian.PutUint32(buf[pos:], uint32(v))
		pos += 4
		return pos, nil

	case "int64":
		v, err := toInt64(value)
		if err != nil {
			return pos, err
		}
		binary.BigEndian.PutUint64(buf[pos:], uint64(v))
		pos += 8
		return pos, nil

	case "float":
		v, err := toFloat64(value)
		if err != nil {
			return pos, err
		}
		binary.BigEndian.PutUint64(buf[pos:], math.Float64bits(v))
		pos += 8
		return pos, nil

	case "bool":
		if toBool(value) {
			buf[pos] = 1
		} else {
			buf[pos] = 0
		}
		pos++
		return pos, nil

	case "geopoint":
		lat, lng, err := toGeoPoint(value)
		if err != nil {
			return pos, err
		}
		binary.BigEndian.PutUint64(buf[pos:], math.Float64bits(lat))
		pos += 8
		binary.BigEndian.PutUint64(buf[pos:], math.Float64bits(lng))
		pos += 8
		return pos, nil

	case "string[]":
		arr := toAnySlice(value)
		binary.BigEndian.PutUint16(buf[pos:], uint16(len(arr)))
		pos += 2
		for _, el := range arr {
			if el == nil {
				buf[pos] = 0
				pos++
			} else {
				buf[pos] = 1
				pos++
				s := toString(el)
				sBytes := []byte(s)
				binary.BigEndian.PutUint32(buf[pos:], uint32(len(sBytes)))
				pos += 4
				copy(buf[pos:], sBytes)
				pos += len(sBytes)
			}
		}
		return pos, nil

	case "int32[]":
		arr := toAnySlice(value)
		binary.BigEndian.PutUint16(buf[pos:], uint16(len(arr)))
		pos += 2
		for _, el := range arr {
			if el == nil {
				buf[pos] = 0
				pos++
			} else {
				buf[pos] = 1
				pos++
				v, err := toInt32(el)
				if err != nil {
					return pos, err
				}
				binary.BigEndian.PutUint32(buf[pos:], uint32(v))
				pos += 4
			}
		}
		return pos, nil

	case "int64[]":
		arr := toAnySlice(value)
		binary.BigEndian.PutUint16(buf[pos:], uint16(len(arr)))
		pos += 2
		for _, el := range arr {
			if el == nil {
				buf[pos] = 0
				pos++
			} else {
				buf[pos] = 1
				pos++
				v, err := toInt64(el)
				if err != nil {
					return pos, err
				}
				binary.BigEndian.PutUint64(buf[pos:], uint64(v))
				pos += 8
			}
		}
		return pos, nil

	case "float[]":
		arr := toAnySlice(value)
		binary.BigEndian.PutUint16(buf[pos:], uint16(len(arr)))
		pos += 2
		for _, el := range arr {
			v, err := toFloat32(el)
			if err != nil {
				return pos, err
			}
			binary.BigEndian.PutUint32(buf[pos:], math.Float32bits(v))
			pos += 4
		}
		return pos, nil

	default:
		return pos, nil
	}
}

// ── Frame I/O ───────────────────────────────────────────────────────────

func readExact(r io.Reader, buf []byte) error {
	_, err := io.ReadFull(r, buf)
	return err
}

func readFrame(conn net.Conn) ([]byte, error) {
	header := make([]byte, 4)
	if err := readExact(conn, header); err != nil {
		return nil, fmt.Errorf("read frame header: %w", err)
	}
	length := binary.BigEndian.Uint32(header)
	if length == 0 {
		return nil, nil
	}
	payload := make([]byte, length)
	if err := readExact(conn, payload); err != nil {
		return nil, fmt.Errorf("read frame payload: %w", err)
	}
	return payload, nil
}

func writeFrame(conn net.Conn, payload []byte) error {
	header := make([]byte, 4)
	binary.BigEndian.PutUint32(header, uint32(len(payload)))
	if _, err := conn.Write(header); err != nil {
		return err
	}
	if _, err := conn.Write(payload); err != nil {
		return err
	}
	return nil
}

// ── Response Decoder ────────────────────────────────────────────────────

func decodeResponse(data []byte) (any, error) {
	if len(data) == 0 {
		return nil, errors.New("empty server response")
	}
	msgType := data[0]
	payload := data[1:]

	switch msgType {
	case msgAck:
		if len(payload) < 16 {
			return nil, errors.New("ack message too short")
		}
		return &BatchAck{
			BatchSeq:     binary.BigEndian.Uint64(payload[0:]),
			DocsReceived: binary.BigEndian.Uint64(payload[8:]),
		}, nil

	case msgCommitResult:
		if len(payload) < 17 {
			return nil, errors.New("commit result message too short")
		}
		return &CommitResult{
			Success:     payload[0] != 0,
			DocsIndexed: binary.BigEndian.Uint64(payload[1:]),
			BuildMs:     binary.BigEndian.Uint64(payload[9:]),
		}, nil

	case msgError:
		if len(payload) < 6 {
			return nil, errors.New("error message too short")
		}
		code := binary.BigEndian.Uint16(payload[0:])
		msgLen := binary.BigEndian.Uint32(payload[2:])
		message := "Unknown error"
		if msgLen > 0 && int(msgLen) <= len(payload)-6 {
			message = string(payload[6 : 6+msgLen])
		}
		return nil, &ProtocolError{Code: code, Message: message}

	case msgBackpressure:
		if len(payload) < 4 {
			return nil, errors.New("backpressure message too short")
		}
		retryMs := binary.BigEndian.Uint32(payload[0:])
		return nil, fmt.Errorf("server backpressure: retry after %dms", retryMs)

	default:
		return nil, fmt.Errorf("unknown server message type: 0x%02x", msgType)
	}
}

// ── IngestClient ────────────────────────────────────────────────────────

// IngestClient is a TCP ingest client for streaming documents to Torque.
type IngestClient struct {
	conn  net.Conn
	state string // "idle" or "streaming"
	enc   DocumentEncoder
}

// Connect opens a TCP connection to the Torque ingest server.
func (c *IngestClient) Connect(host string, port int) error {
	if c.conn != nil {
		return errors.New("already connected")
	}
	conn, err := net.Dial("tcp", net.JoinHostPort(host, fmt.Sprintf("%d", port)))
	if err != nil {
		return fmt.Errorf("connect: %w", err)
	}
	if tc, ok := conn.(*net.TCPConn); ok {
		tc.SetNoDelay(true)
	}
	c.conn = conn
	c.state = "idle"
	return nil
}

// StartIngest begins an ingest session for the given collection.
func (c *IngestClient) StartIngest(collection string) error {
	if c.conn == nil {
		return errors.New("not connected")
	}
	if c.state != "idle" {
		return fmt.Errorf("cannot start ingest: state is %s, expected idle", c.state)
	}

	collBytes := []byte(collection)
	// [0x01][u32 len][coll UTF-8][u8 mode=0]
	payload := make([]byte, 1+4+len(collBytes)+1)
	pos := 0
	payload[pos] = msgStartIngest
	pos++
	binary.BigEndian.PutUint32(payload[pos:], uint32(len(collBytes)))
	pos += 4
	copy(payload[pos:], collBytes)
	pos += len(collBytes)
	payload[pos] = 0 // mode = batch

	if err := writeFrame(c.conn, payload); err != nil {
		return fmt.Errorf("write StartIngest: %w", err)
	}
	resp, err := readFrame(c.conn)
	if err != nil {
		return fmt.Errorf("read StartIngest response: %w", err)
	}
	if _, err := decodeResponse(resp); err != nil {
		return err
	}
	c.state = "streaming"
	return nil
}

// SendBatch sends a batch of documents to the server.
func (c *IngestClient) SendBatch(documents []map[string]any, schema []FieldDef) error {
	if c.conn == nil {
		return errors.New("not connected")
	}
	if c.state != "streaming" {
		return fmt.Errorf("cannot send batch: state is %s, expected streaming", c.state)
	}

	// Encode all documents
	encodedDocs := make([][]byte, len(documents))
	totalDocBytes := 0
	for i, doc := range documents {
		encoded, err := c.enc.Encode(doc, schema)
		if err != nil {
			return fmt.Errorf("encode doc %d: %w", i, err)
		}
		encodedDocs[i] = encoded
		totalDocBytes += len(encoded)
	}

	// DocBatch: [0x02][u32 doc_count][doc bytes...]
	payload := make([]byte, 1+4+totalDocBytes)
	pos := 0
	payload[pos] = msgDocBatch
	pos++
	binary.BigEndian.PutUint32(payload[pos:], uint32(len(documents)))
	pos += 4
	for _, encoded := range encodedDocs {
		copy(payload[pos:], encoded)
		pos += len(encoded)
	}

	if err := writeFrame(c.conn, payload); err != nil {
		c.state = "idle"
		return fmt.Errorf("write DocBatch: %w", err)
	}
	resp, err := readFrame(c.conn)
	if err != nil {
		c.state = "idle"
		return fmt.Errorf("read DocBatch response: %w", err)
	}
	if _, err := decodeResponse(resp); err != nil {
		return err
	}
	return nil
}

// Commit commits the current ingest session. Returns the docs_received count.
func (c *IngestClient) Commit() (uint64, error) {
	if c.conn == nil {
		return 0, errors.New("not connected")
	}
	if c.state != "streaming" {
		return 0, fmt.Errorf("cannot commit: state is %s, expected streaming", c.state)
	}

	payload := []byte{msgCommit}
	if err := writeFrame(c.conn, payload); err != nil {
		c.state = "idle"
		return 0, fmt.Errorf("write Commit: %w", err)
	}
	resp, err := readFrame(c.conn)
	if err != nil {
		c.state = "idle"
		return 0, fmt.Errorf("read Commit response: %w", err)
	}
	result, err := decodeResponse(resp)
	if err != nil {
		c.state = "idle"
		return 0, err
	}
	c.state = "idle"

	switch r := result.(type) {
	case *BatchAck:
		return r.DocsReceived, nil
	case *CommitResult:
		return r.DocsIndexed, nil
	default:
		return 0, nil
	}
}

// Close closes the TCP connection.
func (c *IngestClient) Close() error {
	c.state = "idle"
	if c.conn != nil {
		err := c.conn.Close()
		c.conn = nil
		return err
	}
	return nil
}

// ── BinaryWriter ────────────────────────────────────────────────────────

// BinaryWriter writes documents to a TQBF (Torque Binary File) with zlib compression.
type BinaryWriter struct {
	schema    []FieldDef
	filePath  string
	raw       []byte
	docCount  uint32
	finalized bool
	enc       DocumentEncoder
}

// NewBinaryWriter creates a new TQBF file writer.
func NewBinaryWriter(schema []FieldDef, filePath string) *BinaryWriter {
	return &BinaryWriter{
		schema:   schema,
		filePath: filePath,
	}
}

// DocumentCount returns the number of documents written so far.
func (w *BinaryWriter) DocumentCount() uint32 { return w.docCount }

// RawSize returns the raw (uncompressed) payload size in bytes.
func (w *BinaryWriter) RawSize() int { return len(w.raw) }

// Write encodes and appends a single document.
func (w *BinaryWriter) Write(doc map[string]any) error {
	if w.finalized {
		return errors.New("writer has been finalized")
	}
	encoded, err := w.enc.Encode(doc, w.schema)
	if err != nil {
		return err
	}
	w.raw = append(w.raw, encoded...)
	w.docCount++
	return nil
}

// Finalize compresses with zlib and writes the TQBF file to disk.
// Returns the total file size in bytes.
func (w *BinaryWriter) Finalize() (int64, error) {
	if w.finalized {
		info, err := os.Stat(w.filePath)
		if err != nil {
			return 0, err
		}
		return info.Size(), nil
	}
	w.finalized = true

	f, err := os.Create(w.filePath)
	if err != nil {
		return 0, fmt.Errorf("create file: %w", err)
	}
	defer f.Close()

	// Header (TQBF v2, 24 bytes):
	//   magic(4) + version(4 LE) + doc_count(4 LE) + flags(4 LE) + uncompressed_size(8 LE)
	header := make([]byte, 24)
	copy(header[0:], "TQBF")
	binary.LittleEndian.PutUint32(header[4:], tqbfVersion)
	binary.LittleEndian.PutUint32(header[8:], w.docCount)
	binary.LittleEndian.PutUint32(header[12:], flagZlib)
	binary.LittleEndian.PutUint64(header[16:], uint64(len(w.raw)))
	if _, err := f.Write(header); err != nil {
		return 0, fmt.Errorf("write header: %w", err)
	}

	// Zlib-compressed payload
	zw, err := zlib.NewWriterLevel(f, zlib.BestCompression)
	if err != nil {
		return 0, fmt.Errorf("create zlib writer: %w", err)
	}
	if _, err := zw.Write(w.raw); err != nil {
		return 0, fmt.Errorf("write compressed payload: %w", err)
	}
	if err := zw.Close(); err != nil {
		return 0, fmt.Errorf("close zlib: %w", err)
	}

	// Free memory
	w.raw = nil

	info, err := f.Stat()
	if err != nil {
		return 0, err
	}
	return info.Size(), nil
}

// ── Type conversion helpers ─────────────────────────────────────────────

func toString(v any) string {
	if v == nil {
		return ""
	}
	switch s := v.(type) {
	case string:
		return s
	default:
		return fmt.Sprintf("%v", v)
	}
}

func toInt32(v any) (int32, error) {
	switch n := v.(type) {
	case int:
		return int32(n), nil
	case int32:
		return n, nil
	case int64:
		return int32(n), nil
	case float64:
		return int32(n), nil
	case float32:
		return int32(n), nil
	default:
		return 0, fmt.Errorf("cannot convert %T to int32", v)
	}
}

func toInt64(v any) (int64, error) {
	switch n := v.(type) {
	case int:
		return int64(n), nil
	case int32:
		return int64(n), nil
	case int64:
		return n, nil
	case float64:
		return int64(n), nil
	case float32:
		return int64(n), nil
	default:
		return 0, fmt.Errorf("cannot convert %T to int64", v)
	}
}

func toFloat64(v any) (float64, error) {
	switch n := v.(type) {
	case float64:
		return n, nil
	case float32:
		return float64(n), nil
	case int:
		return float64(n), nil
	case int32:
		return float64(n), nil
	case int64:
		return float64(n), nil
	default:
		return 0, fmt.Errorf("cannot convert %T to float64", v)
	}
}

func toFloat32(v any) (float32, error) {
	switch n := v.(type) {
	case float32:
		return n, nil
	case float64:
		return float32(n), nil
	case int:
		return float32(n), nil
	case int32:
		return float32(n), nil
	case int64:
		return float32(n), nil
	default:
		return 0, fmt.Errorf("cannot convert %T to float32", v)
	}
}

func toBool(v any) bool {
	switch b := v.(type) {
	case bool:
		return b
	default:
		return false
	}
}

func toGeoPoint(v any) (float64, float64, error) {
	switch g := v.(type) {
	case GeoPoint:
		return g.Lat, g.Lng, nil
	case *GeoPoint:
		return g.Lat, g.Lng, nil
	case []any:
		if len(g) >= 2 {
			lat, err1 := toFloat64(g[0])
			lng, err2 := toFloat64(g[1])
			if err1 != nil || err2 != nil {
				return 0, 0, fmt.Errorf("invalid geopoint array values")
			}
			return lat, lng, nil
		}
		return 0, 0, fmt.Errorf("geopoint array must have 2 elements")
	case map[string]any:
		lat, err1 := toFloat64(g["lat"])
		lng, err2 := toFloat64(g["lng"])
		if err1 != nil || err2 != nil {
			return 0, 0, fmt.Errorf("invalid geopoint map values")
		}
		return lat, lng, nil
	default:
		return 0, 0, fmt.Errorf("cannot convert %T to geopoint", v)
	}
}

func toAnySlice(v any) []any {
	switch s := v.(type) {
	case []any:
		return s
	case []string:
		result := make([]any, len(s))
		for i, el := range s {
			result[i] = el
		}
		return result
	case []int:
		result := make([]any, len(s))
		for i, el := range s {
			result[i] = el
		}
		return result
	case []int32:
		result := make([]any, len(s))
		for i, el := range s {
			result[i] = el
		}
		return result
	case []int64:
		result := make([]any, len(s))
		for i, el := range s {
			result[i] = el
		}
		return result
	case []float32:
		result := make([]any, len(s))
		for i, el := range s {
			result[i] = el
		}
		return result
	case []float64:
		result := make([]any, len(s))
		for i, el := range s {
			result[i] = el
		}
		return result
	default:
		return nil
	}
}
