"""Torque TCP binary ingest client and TQBF writer.

Uses only Python stdlib: socket, struct, zlib, io. Zero external dependencies.

Wire format matches the .NET SDK's TorqueIngestClient and the Rust server's
ingest protocol (src/ingest/protocol.rs). All multi-byte integers are big-endian.
"""

from __future__ import annotations

import socket
import struct
import zlib
from io import BytesIO
from dataclasses import dataclass
from typing import Any

# ── Protocol Constants ──────────────────────────────────────────────────

# Client -> Server message types
MSG_START_INGEST = 0x01
MSG_DOC_BATCH = 0x02
MSG_COMMIT = 0x03
MSG_ABORT = 0x04

# Server -> Client message types
RESP_ACK = 0x10
RESP_COMMIT_RESULT = 0x11
RESP_ERROR = 0x12
RESP_BACKPRESSURE = 0x13

# ── Response Models ─────────────────────────────────────────────────────

@dataclass
class BatchAck:
    batch_seq: int
    docs_received: int

@dataclass
class CommitResult:
    success: bool
    docs_indexed: int
    build_ms: int

# ── Exceptions ──────────────────────────────────────────────────────────

class TorqueProtocolError(Exception):
    def __init__(self, code: int, message: str) -> None:
        super().__init__(f"[code={code}] {message}")
        self.code = code

class TorqueConnectionError(Exception):
    pass

# ── Frame Codec ─────────────────────────────────────────────────────────

def _write_frame(sock: socket.socket, payload: bytes) -> None:
    header = struct.pack("!I", len(payload))
    sock.sendall(header + payload)

def _read_exact(sock: socket.socket, n: int) -> bytes:
    buf = bytearray()
    while len(buf) < n:
        chunk = sock.recv(n - len(buf))
        if not chunk:
            raise TorqueConnectionError("Connection closed while reading frame")
        buf.extend(chunk)
    return bytes(buf)

def _read_frame(sock: socket.socket) -> bytes:
    header = _read_exact(sock, 4)
    length = struct.unpack("!I", header)[0]
    if length == 0:
        return b""
    return _read_exact(sock, length)

# ── Response Decoder ────────────────────────────────────────────────────

def _decode_response(data: bytes) -> BatchAck | CommitResult:
    if not data:
        raise TorqueProtocolError(0, "Empty server message")

    msg_type = data[0]
    payload = data[1:]

    if msg_type == RESP_ACK:
        if len(payload) < 16:
            raise TorqueProtocolError(0, "Ack message too short")
        batch_seq, docs_received = struct.unpack_from("!QQ", payload)
        return BatchAck(batch_seq=batch_seq, docs_received=docs_received)

    elif msg_type == RESP_COMMIT_RESULT:
        if len(payload) < 17:
            raise TorqueProtocolError(0, "CommitResult message too short")
        success = payload[0] != 0
        docs_indexed, build_ms = struct.unpack_from("!QQ", payload, 1)
        return CommitResult(success=success, docs_indexed=docs_indexed, build_ms=build_ms)

    elif msg_type == RESP_ERROR:
        if len(payload) < 6:
            raise TorqueProtocolError(0, "Error message too short")
        code = struct.unpack_from("!H", payload, 0)[0]
        msg_len = struct.unpack_from("!I", payload, 2)[0]
        message = "Unknown error"
        if msg_len > 0 and msg_len <= len(payload) - 6:
            message = payload[6:6 + msg_len].decode("utf-8", errors="replace")
        raise TorqueProtocolError(code, message)

    elif msg_type == RESP_BACKPRESSURE:
        if len(payload) < 4:
            raise TorqueProtocolError(0, "Backpressure message too short")
        retry_after_ms = struct.unpack_from("!I", payload, 0)[0]
        raise TorqueProtocolError(
            8, f"Server backpressure, retry after {retry_after_ms}ms"
        )

    else:
        raise TorqueProtocolError(0, f"Unknown server message type: 0x{msg_type:02X}")

# ── Document Flattener ─────────────────────────────────────────────────

def flatten_document(doc: dict[str, Any]) -> dict[str, Any]:
    """Flatten nested objects/arrays in a document to dotted keys.

    Matches the Rust server's ``flatten_json_object`` logic:
    - Nested objects become dotted keys: ``{"a": {"b": 1}}`` -> ``{"a.b": 1}``
    - Arrays of objects are pivoted: ``{"t": [{"n": "x"}, {"n": "y"}]}``
      -> ``{"t.n": ["x", "y"]}`` with null-padding for missing fields.
    - Scalars and flat arrays pass through unchanged.
    """
    return _flatten_inner(doc, 0)

def _flatten_inner(obj: dict[str, Any], depth: int) -> dict[str, Any]:

    result: dict[str, Any] = {}
    for key, value in obj.items():
        if isinstance(value, dict):
            flat_inner = _flatten_inner(value, depth + 1)
            for inner_key, inner_val in flat_inner.items():
                result[f"{key}.{inner_key}"] = inner_val
        elif (
            isinstance(value, list)
            and len(value) > 0
            and isinstance(value[0], dict)
        ):
            # Collect all field names across all elements (preserving order)
            seen: set[str] = set()
            all_field_names: list[str] = []
            for item in value:
                if isinstance(item, dict):
                    for field_name in item:
                        if field_name not in seen:
                            seen.add(field_name)
                            all_field_names.append(field_name)
            # Build arrays with null-padding for missing fields
            for field_name in all_field_names:
                arr_key = f"{key}.{field_name}"
                values = []
                for item in value:
                    if isinstance(item, dict):
                        values.append(item.get(field_name))
                    else:
                        values.append(None)
                result[arr_key] = values
        else:
            result[key] = value

    return result

# ── Document Encoder ────────────────────────────────────────────────────

# Type name -> encoder key mapping
_TYPE_MAP = {
    "string": "string",
    "int32": "int32",
    "int64": "int64",
    "float": "float",
    "bool": "bool",
    "geopoint": "geopoint",
    "string[]": "string[]",
    "int32[]": "int32[]",
    "int64[]": "int64[]",
    "float[]": "float[]",
}

class DocumentEncoder:
    """Encodes Python dicts into Torque binary wire format.

    Wire format per document:
        [u16 BE] id_byte_length
        [bytes]  id (UTF-8)
        [u8]     language (0 = English)
        [u16 BE] field_count
        For each field in schema order:
            [u8] presence (0 = null, 1 = present)
            If present: encoded value (type-specific)

    Field encoding:
        string:   [u32 BE length][UTF-8 bytes]
        int32:    [i32 BE]
        int64:    [i64 BE]
        float:    [f64 BE] (8 bytes double)
        bool:     [u8] (0 or 1)
        geopoint: [f64 BE lat][f64 BE lng]
        string[]: [u16 BE count] then per element: [u8 null_flag] if present: [u32 BE len][UTF-8]
        int32[]:  [u16 BE count] then per element: [u8 null_flag] if present: [i32 BE]
        int64[]:  [u16 BE count] then per element: [u8 null_flag] if present: [i64 BE]
        float[]:  [u16 BE count] then per element: [f32 BE] (4 bytes, no null flag)
    """

    # Language codes matching Rust enum
    LANG_ENGLISH = 0
    LANG_SWEDISH = 1
    LANG_NORWEGIAN = 2
    LANG_DANISH = 3
    LANG_FINNISH = 4
    LANG_GERMAN = 5
    LANG_FRENCH = 6
    LANG_SPANISH = 7
    LANG_PORTUGUESE = 8
    LANG_ITALIAN = 9
    LANG_DUTCH = 10
    LANG_RUSSIAN = 11
    LANG_ARABIC = 12
    LANG_GREEK = 13
    LANG_HUNGARIAN = 14
    LANG_ROMANIAN = 15
    LANG_TURKISH = 16

    LANG_MAP = {
        "en": LANG_ENGLISH,
        "sv": LANG_SWEDISH,
        "no": LANG_NORWEGIAN,
        "da": LANG_DANISH,
        "fi": LANG_FINNISH,
        "de": LANG_GERMAN,
        "fr": LANG_FRENCH,
        "es": LANG_SPANISH,
        "pt": LANG_PORTUGUESE,
        "it": LANG_ITALIAN,
        "nl": LANG_DUTCH,
        "ru": LANG_RUSSIAN,
        "ar": LANG_ARABIC,
        "el": LANG_GREEK,
        "hu": LANG_HUNGARIAN,
        "ro": LANG_ROMANIAN,
        "tr": LANG_TURKISH,
    }

    def __init__(self, schema: dict[str, Any]) -> None:
        self._fields: list[tuple[str, str]] = []
        for f in schema.get("fields", []):
            name = f["name"]
            ftype = f["type"]
            if ftype not in _TYPE_MAP:
                raise ValueError(f"Unsupported field type: {ftype!r} for field {name!r}")
            self._fields.append((name, _TYPE_MAP[ftype]))

    def encode(self, doc: dict[str, Any], language: str = "en") -> bytes:
        doc = flatten_document(doc)
        buf = BytesIO()

        # External ID
        doc_id = str(doc.get("id", ""))
        id_bytes = doc_id.encode("utf-8")
        buf.write(struct.pack("!H", len(id_bytes)))
        buf.write(id_bytes)

        # Language
        lang_code = self.LANG_MAP.get(language, self.LANG_ENGLISH)
        buf.write(struct.pack("B", lang_code))

        # Field count
        buf.write(struct.pack("!H", len(self._fields)))

        # Fields in schema order
        for field_name, field_type in self._fields:
            value = doc.get(field_name)
            if value is None:
                buf.write(b"\x00")  # null
            else:
                buf.write(b"\x01")  # present
                self._encode_value(buf, value, field_type)

        return buf.getvalue()

    def encode_batch(self, docs: list[dict[str, Any]], language: str = "en") -> bytes:
        buf = BytesIO()
        for doc in docs:
            buf.write(self.encode(doc, language))
        return buf.getvalue()

    @staticmethod
    def _encode_value(buf: BytesIO, value: Any, field_type: str) -> None:
        if field_type == "string":
            s = str(value).encode("utf-8")
            buf.write(struct.pack("!I", len(s)))
            buf.write(s)

        elif field_type == "int32":
            buf.write(struct.pack("!i", int(value)))

        elif field_type == "int64":
            buf.write(struct.pack("!q", int(value)))

        elif field_type == "float":
            buf.write(struct.pack("!d", float(value)))

        elif field_type == "bool":
            buf.write(b"\x01" if value else b"\x00")

        elif field_type == "geopoint":
            # Accepts [lat, lng] or (lat, lng)
            lat, lng = float(value[0]), float(value[1])
            buf.write(struct.pack("!dd", lat, lng))

        elif field_type == "string[]":
            arr = list(value)
            buf.write(struct.pack("!H", len(arr)))
            for item in arr:
                if item is None:
                    buf.write(b"\x00")
                else:
                    buf.write(b"\x01")
                    s = str(item).encode("utf-8")
                    buf.write(struct.pack("!I", len(s)))
                    buf.write(s)

        elif field_type == "int32[]":
            arr = list(value)
            buf.write(struct.pack("!H", len(arr)))
            for item in arr:
                if item is None:
                    buf.write(b"\x00")
                else:
                    buf.write(b"\x01")
                    buf.write(struct.pack("!i", int(item)))

        elif field_type == "int64[]":
            arr = list(value)
            buf.write(struct.pack("!H", len(arr)))
            for item in arr:
                if item is None:
                    buf.write(b"\x00")
                else:
                    buf.write(b"\x01")
                    buf.write(struct.pack("!q", int(item)))

        elif field_type == "float[]":
            arr = list(value)
            buf.write(struct.pack("!H", len(arr)))
            for item in arr:
                buf.write(struct.pack("!f", float(item)))

# ── TCP Ingest Client ───────────────────────────────────────────────────

class TorqueIngestClient:
    """TCP ingest client for streaming documents to Torque via the binary protocol.

    Usage:
        encoder = DocumentEncoder(schema)
        with TorqueIngestClient() as client:
            client.connect("localhost", 8119)
            client.start_ingest("my_collection")
            client.send_batch(documents, encoder)
            result = client.commit()
            print(f"Indexed {result.docs_indexed} docs in {result.build_ms}ms")
    """

    def __init__(self, *, recv_buf_size: int = 4096) -> None:
        self._sock: socket.socket | None = None
        self._recv_buf_size = recv_buf_size

    def __enter__(self) -> TorqueIngestClient:
        return self

    def __exit__(self, *args: Any) -> None:
        self.close()

    def connect(self, host: str, port: int, *, timeout: float = 30.0) -> None:
        if self._sock is not None:
            raise RuntimeError("Already connected")
        self._sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        self._sock.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
        self._sock.settimeout(timeout)
        try:
            self._sock.connect((host, port))
        except OSError as e:
            self._sock.close()
            self._sock = None
            raise TorqueConnectionError(
                f"Failed to connect to {host}:{port}: {e}"
            ) from e

    def start_ingest(self, collection: str, *, mode: int = 0) -> BatchAck:
        self._ensure_connected()
        # Encode: [0x01][u32 BE coll_len][coll UTF-8][u8 mode]
        coll_bytes = collection.encode("utf-8")
        payload = (
            struct.pack("B", MSG_START_INGEST)
            + struct.pack("!I", len(coll_bytes))
            + coll_bytes
            + struct.pack("B", mode)
        )
        _write_frame(self._sock, payload)
        data = _read_frame(self._sock)
        result = _decode_response(data)
        if not isinstance(result, BatchAck):
            raise TorqueProtocolError(0, f"Expected Ack, got {type(result).__name__}")
        return result

    def send_batch(
        self,
        documents: list[dict[str, Any]],
        encoder: DocumentEncoder,
        *,
        language: str = "en",
    ) -> BatchAck:
        self._ensure_connected()
        # Encode: [0x02][u32 BE doc_count][encoded docs...]
        doc_payload = encoder.encode_batch(documents, language)
        header = (
            struct.pack("B", MSG_DOC_BATCH)
            + struct.pack("!I", len(documents))
        )
        payload = header + doc_payload
        _write_frame(self._sock, payload)
        data = _read_frame(self._sock)
        result = _decode_response(data)
        if not isinstance(result, BatchAck):
            raise TorqueProtocolError(0, f"Expected Ack, got {type(result).__name__}")
        return result

    def commit(self) -> BatchAck:
        """Send Commit. Server responds with Ack (index builds async)."""
        self._ensure_connected()
        payload = struct.pack("B", MSG_COMMIT)
        _write_frame(self._sock, payload)
        data = _read_frame(self._sock)
        result = _decode_response(data)
        if isinstance(result, (BatchAck, CommitResult)):
            return BatchAck(batch_seq=getattr(result, 'batch_seq', 0),
                          docs_received=getattr(result, 'docs_received', getattr(result, 'docs_indexed', 0)))
        raise TorqueProtocolError(0, f"Expected Ack, got {type(result).__name__}")

    def abort(self) -> BatchAck:
        self._ensure_connected()
        payload = struct.pack("B", MSG_ABORT)
        _write_frame(self._sock, payload)
        data = _read_frame(self._sock)
        result = _decode_response(data)
        if not isinstance(result, BatchAck):
            raise TorqueProtocolError(0, f"Expected Ack, got {type(result).__name__}")
        return result

    def send_documents(
        self,
        documents: list[dict[str, Any]],
        encoder: DocumentEncoder,
        *,
        batch_size: int = 50_000,
        language: str = "en",
    ) -> BatchAck:
        last_ack = None
        for offset in range(0, len(documents), batch_size):
            chunk = documents[offset:offset + batch_size]
            last_ack = self.send_batch(chunk, encoder, language=language)
        if last_ack is None:
            raise ValueError("No documents to send")
        return last_ack

    def close(self) -> None:
        if self._sock is not None:
            try:
                self._sock.close()
            except OSError:
                pass
            self._sock = None

    def _ensure_connected(self) -> None:
        if self._sock is None:
            raise RuntimeError("Not connected. Call connect() first.")

# ── TQBF Binary File Writer ────────────────────────────────────────────

class TorqueBinaryFileWriter:
    """Streaming binary file writer for TQBF format.

    Encodes documents using DocumentEncoder for proper binary encoding,
    then ZLib-compresses and writes a TQBF v2 file.

    File format (TQBF v2):
        [4 bytes] magic: "TQBF"
        [4 bytes] version: 2 (LE)
        [4 bytes] doc_count: u32 (LE)
        [4 bytes] flags: 2 = ZLib compressed binary
        [8 bytes] uncompressed_size: u64 (LE)  ← v2 only
        [N bytes] payload: ZLib-compressed concatenated binary documents

    The uncompressed_size field lets the server size its decompression
    buffer exactly once.

    The torque server still accepts v1 files (16-byte header, no size field)
    via the read_to_end fallback, so older clients keep working.

    Usage:
        encoder = DocumentEncoder(schema)
        with TorqueBinaryFileWriter("data.tqbf", encoder) as writer:
            for doc in data_source:
                writer.write(doc)
        # Upload via client.import_binary("collection", "data.tqbf")
    """

    def __init__(self, file_path: str, encoder: DocumentEncoder) -> None:
        self._file_path = file_path
        self._encoder = encoder
        self._buffer = BytesIO()
        self._doc_count = 0
        self._finalized = False

    def __enter__(self) -> TorqueBinaryFileWriter:
        return self

    def __exit__(self, *args: Any) -> None:
        self.finalize()

    @property
    def document_count(self) -> int:
        return self._doc_count

    @property
    def raw_size(self) -> int:
        return self._buffer.tell()

    def write(self, document: dict[str, Any], language: str = "en") -> None:
        if self._finalized:
            raise RuntimeError("Writer has been finalized.")
        encoded = self._encoder.encode(document, language)
        self._buffer.write(encoded)
        self._doc_count += 1

    def finalize(self) -> str:
        if self._finalized:
            return self._file_path
        self._finalized = True

        raw = self._buffer.getvalue()
        uncompressed_size = len(raw)
        compressed = zlib.compress(raw, level=6)

        with open(self._file_path, "wb") as f:
            f.write(b"TQBF")
            f.write(struct.pack("<I", 2))                       # version: 2
            f.write(struct.pack("<I", self._doc_count))         # doc_count
            f.write(struct.pack("<I", 2))                       # flags: 2 = ZLib binary
            f.write(struct.pack("<Q", uncompressed_size))       # NEW: uncompressed size (u64)
            f.write(compressed)

        return self._file_path
