import { createConnection } from 'node:net';
import { deflateSync } from 'node:zlib';
import { writeFileSync } from 'node:fs';
import { Buffer } from 'node:buffer';

// ── Protocol Constants ──────────────────────────────────────────────────

const MSG_START_INGEST = 0x01;
const MSG_DOC_BATCH    = 0x02;
const MSG_COMMIT       = 0x03;

const MSG_ACK            = 0x10;
const MSG_COMMIT_RESULT  = 0x11;
const MSG_ERROR          = 0x12;
const MSG_BACKPRESSURE   = 0x13;

const TQBF_MAGIC   = Buffer.from('TQBF', 'ascii');
const TQBF_VERSION = 2;
const FLAG_ZLIB    = 2;

// ── Language Codes ─────────────────────────────────────────────────────

/** Language code to wire byte mapping (matches Rust Language enum). */
export const LANG_MAP = {
  en: 0,   // English
  sv: 1,   // Swedish
  no: 2,   // Norwegian
  da: 3,   // Danish
  fi: 4,   // Finnish
  de: 5,   // German
  fr: 6,   // French
  es: 7,   // Spanish
  pt: 8,   // Portuguese
  it: 9,   // Italian
  nl: 10,  // Dutch
  ru: 11,  // Russian
  ar: 12,  // Arabic
  el: 13,  // Greek
  hu: 14,  // Hungarian
  ro: 15,  // Romanian
  tr: 16,  // Turkish
};

// ── Document Flattener ─────────────────────────────────────────────────


/**
 * Flatten nested objects/arrays to dotted keys matching the Rust server's
 * flatten_json_object logic.
 * @param {Object} doc
 * @returns {Object}
 */
export function flattenDocument(doc) {
  return flattenInner(doc, 0);
}

function flattenInner(obj, depth) {

  const result = {};
  for (const [key, value] of Object.entries(obj)) {
    if (value !== null && typeof value === 'object' && !Array.isArray(value)) {
      // Nested object: flatten with dotted prefix
      const flatInner = flattenInner(value, depth + 1);
      for (const [innerKey, innerVal] of Object.entries(flatInner)) {
        result[`${key}.${innerKey}`] = innerVal;
      }
    } else if (
      Array.isArray(value) &&
      value.length > 0 &&
      value[0] !== null &&
      typeof value[0] === 'object' &&
      !Array.isArray(value[0])
    ) {
      // Array of objects: pivot to object of arrays
      const seen = new Set();
      const allFieldNames = [];
      for (const item of value) {
        if (item !== null && typeof item === 'object' && !Array.isArray(item)) {
          for (const fieldName of Object.keys(item)) {
            if (!seen.has(fieldName)) {
              seen.add(fieldName);
              allFieldNames.push(fieldName);
            }
          }
        }
      }
      for (const fieldName of allFieldNames) {
        const arrKey = `${key}.${fieldName}`;
        const values = [];
        for (const item of value) {
          if (item !== null && typeof item === 'object' && !Array.isArray(item)) {
            values.push(item[fieldName] ?? null);
          } else {
            values.push(null);
          }
        }
        result[arrKey] = values;
      }
    } else {
      result[key] = value;
    }
  }
  return result;
}

// ── DocumentEncoder ─────────────────────────────────────────────────────

export class DocumentEncoder {
  /**
   * Encode a single document into a Buffer.
   * @param {Object} doc - Document with "id" key and field values.
   * @param {{ name: string, type: string }[]} schema - Ordered field definitions.
   * @returns {Buffer}
   */
  static encode(doc, schema) {
    doc = flattenDocument(doc);
    const size = DocumentEncoder.computeSize(doc, schema);
    const buf = Buffer.allocUnsafe(size);
    let pos = 0;

    // External ID (u16 BE length + UTF-8)
    const id = String(doc.id ?? '');
    const idBytes = Buffer.from(id, 'utf8');
    buf.writeUInt16BE(idBytes.length, pos); pos += 2;
    idBytes.copy(buf, pos); pos += idBytes.length;

    // Language byte (default 0 = English)
    buf[pos++] = doc._language ?? 0;

    // Field count (u16 BE)
    buf.writeUInt16BE(schema.length, pos); pos += 2;

    // Fields in schema order
    for (const field of schema) {
      const value = doc[field.name];
      if (value === undefined || value === null) {
        buf[pos++] = 0; // null
      } else {
        buf[pos++] = 1; // present
        pos = writeFieldValue(buf, pos, value, field.type);
      }
    }

    return buf.subarray(0, pos);
  }

  /**
   * Compute the exact encoded byte size of a document.
   * @param {Object} doc
   * @param {{ name: string, type: string }[]} schema
   * @returns {number}
   */
  static computeSize(doc, schema) {
    const id = String(doc.id ?? '');
    const idByteLen = Buffer.byteLength(id, 'utf8');
    // u16 id_len + id_bytes + u8 language + u16 field_count
    let size = 2 + idByteLen + 1 + 2;

    for (const field of schema) {
      size += 1; // presence byte
      const value = doc[field.name];
      if (value !== undefined && value !== null) {
        size += computeFieldSize(value, field.type);
      }
    }
    return size;
  }
}

function computeFieldSize(value, type) {
  switch (type) {
    case 'string':
      return 4 + Buffer.byteLength(String(value), 'utf8');
    case 'int32':
      return 4;
    case 'int64':
      return 8;
    case 'float':
      return 8;
    case 'bool':
      return 1;
    case 'geopoint': {
      return 16; // 2x f64
    }
    case 'string[]': {
      const arr = value;
      let s = 2; // u16 array length
      for (const el of arr) {
        s += 1; // null flag
        if (el !== null && el !== undefined) {
          s += 4 + Buffer.byteLength(String(el), 'utf8');
        }
      }
      return s;
    }
    case 'int32[]': {
      const arr = value;
      let s = 2;
      for (const el of arr) {
        s += 1;
        if (el !== null && el !== undefined) s += 4;
      }
      return s;
    }
    case 'int64[]': {
      const arr = value;
      let s = 2;
      for (const el of arr) {
        s += 1;
        if (el !== null && el !== undefined) s += 8;
      }
      return s;
    }
    case 'float[]': {
      return 2 + value.length * 4; // u16 len + f32 per element
    }
    default:
      return 0;
  }
}

function writeFieldValue(buf, pos, value, type) {
  switch (type) {
    case 'string': {
      const strBytes = Buffer.from(String(value), 'utf8');
      buf.writeUInt32BE(strBytes.length, pos); pos += 4;
      strBytes.copy(buf, pos); pos += strBytes.length;
      return pos;
    }
    case 'int32': {
      buf.writeInt32BE(value | 0, pos); pos += 4;
      return pos;
    }
    case 'int64': {
      buf.writeBigInt64BE(BigInt(value), pos); pos += 8;
      return pos;
    }
    case 'float': {
      buf.writeDoubleBE(Number(value), pos); pos += 8;
      return pos;
    }
    case 'bool': {
      buf[pos++] = value ? 1 : 0;
      return pos;
    }
    case 'geopoint': {
      // value is [lat, lng] or { lat, lng }
      const lat = Array.isArray(value) ? value[0] : value.lat;
      const lng = Array.isArray(value) ? value[1] : value.lng;
      buf.writeDoubleBE(lat, pos); pos += 8;
      buf.writeDoubleBE(lng, pos); pos += 8;
      return pos;
    }
    case 'string[]': {
      const arr = value;
      buf.writeUInt16BE(arr.length, pos); pos += 2;
      for (const el of arr) {
        if (el === null || el === undefined) {
          buf[pos++] = 0;
        } else {
          buf[pos++] = 1;
          const elBytes = Buffer.from(String(el), 'utf8');
          buf.writeUInt32BE(elBytes.length, pos); pos += 4;
          elBytes.copy(buf, pos); pos += elBytes.length;
        }
      }
      return pos;
    }
    case 'int32[]': {
      const arr = value;
      buf.writeUInt16BE(arr.length, pos); pos += 2;
      for (const el of arr) {
        if (el === null || el === undefined) {
          buf[pos++] = 0;
        } else {
          buf[pos++] = 1;
          buf.writeInt32BE(el | 0, pos); pos += 4;
        }
      }
      return pos;
    }
    case 'int64[]': {
      const arr = value;
      buf.writeUInt16BE(arr.length, pos); pos += 2;
      for (const el of arr) {
        if (el === null || el === undefined) {
          buf[pos++] = 0;
        } else {
          buf[pos++] = 1;
          buf.writeBigInt64BE(BigInt(el), pos); pos += 8;
        }
      }
      return pos;
    }
    case 'float[]': {
      const arr = value;
      buf.writeUInt16BE(arr.length, pos); pos += 2;
      for (const el of arr) {
        buf.writeFloatBE(el, pos); pos += 4;
      }
      return pos;
    }
    default:
      return pos;
  }
}

// ── Frame I/O helpers ───────────────────────────────────────────────────

function readExact(socket, numBytes) {
  // Use socket.read() with readable events — avoids race condition where
  // data arrives between writeFrame completing and listener registration.
  return new Promise((resolve, reject) => {
    const tryRead = () => {
      const data = socket.read(numBytes);
      if (data !== null && data.length >= numBytes) {
        socket.removeListener('readable', tryRead);
        socket.removeListener('error', onError);
        socket.removeListener('close', onClose);
        resolve(data);
        return;
      }
      if (data !== null) {
        // Partial read — push back and wait for more
        socket.unshift(data);
      }
    };
    const onError = (err) => {
      socket.removeListener('readable', tryRead);
      socket.removeListener('close', onClose);
      reject(err);
    };
    const onClose = () => {
      socket.removeListener('readable', tryRead);
      socket.removeListener('error', onError);
      reject(new Error('Connection closed while reading'));
    };

    socket.on('readable', tryRead);
    socket.on('error', onError);
    socket.on('close', onClose);
    // Try immediately in case data is already buffered
    tryRead();
  });
}

async function readFrame(socket) {
  const header = await readExact(socket, 4);
  const payloadLen = header.readUInt32BE(0);
  if (payloadLen === 0) return Buffer.alloc(0);
  return readExact(socket, payloadLen);
}

function writeFrame(socket, payload) {
  return new Promise((resolve, reject) => {
    const header = Buffer.allocUnsafe(4);
    header.writeUInt32BE(payload.length, 0);
    socket.write(header, (err) => {
      if (err) return reject(err);
      socket.write(payload, (err2) => {
        if (err2) return reject(err2);
        resolve();
      });
    });
  });
}

// ── Response Decoder ────────────────────────────────────────────────────

function decodeResponse(data) {
  if (data.length === 0) throw new Error('Empty server response');
  const msgType = data[0];
  const payload = data.subarray(1);

  switch (msgType) {
    case MSG_ACK: {
      if (payload.length < 16) throw new Error('Ack message too short');
      return {
        type: 'ack',
        batchSeq: payload.readBigUInt64BE(0),
        docsReceived: payload.readBigUInt64BE(8),
      };
    }
    case MSG_COMMIT_RESULT: {
      if (payload.length < 17) throw new Error('CommitResult message too short');
      return {
        type: 'commit_result',
        success: payload[0] !== 0,
        docsIndexed: payload.readBigUInt64BE(1),
        buildMs: payload.readBigUInt64BE(9),
      };
    }
    case MSG_ERROR: {
      if (payload.length < 6) throw new Error('Error message too short');
      const code = payload.readUInt16BE(0);
      const msgLen = payload.readUInt32BE(2);
      const message = msgLen > 0 && msgLen <= payload.length - 6
        ? payload.subarray(6, 6 + msgLen).toString('utf8')
        : 'Unknown error';
      const err = new Error(`Torque protocol error ${code}: ${message}`);
      err.code = code;
      throw err;
    }
    case MSG_BACKPRESSURE: {
      if (payload.length < 4) throw new Error('Backpressure message too short');
      return {
        type: 'backpressure',
        retryAfterMs: payload.readUInt32BE(0),
      };
    }
    default:
      throw new Error(`Unknown server message type: 0x${msgType.toString(16)}`);
  }
}

// ── TorqueIngestClient ──────────────────────────────────────────────────

export class TorqueIngestClient {
  constructor() {
    this._socket = null;
    this._state = 'idle'; // idle | streaming
  }

  /**
   * Connect to the Torque TCP ingest server.
   * @param {string} host
   * @param {number} port
   */
  async connect(host, port) {
    if (this._socket) throw new Error('Already connected');
    return new Promise((resolve, reject) => {
      const sock = createConnection({ host, port }, () => {
        sock.setNoDelay(true);
        sock.pause(); // Switch to paused mode for readable-based reads
        this._socket = sock;
        resolve();
      });
      sock.on('error', reject);
    });
  }

  /**
   * Start an ingest session for the given collection.
   * @param {string} collection
   * @returns {Promise<{ type: string, batchSeq: bigint, docsReceived: bigint }>}
   */
  async startIngest(collection) {
    this._ensureConnected();
    if (this._state !== 'idle') throw new Error(`Cannot start ingest: state is ${this._state}, expected idle`);

    // Encode StartIngest: [0x01][u32 len][collection UTF-8][u8 mode=0]
    const collBytes = Buffer.from(collection, 'utf8');
    const payload = Buffer.allocUnsafe(1 + 4 + collBytes.length + 1);
    let pos = 0;
    payload[pos++] = MSG_START_INGEST;
    payload.writeUInt32BE(collBytes.length, pos); pos += 4;
    collBytes.copy(payload, pos); pos += collBytes.length;
    payload[pos++] = 0; // mode = batch

    await writeFrame(this._socket, payload);
    const resp = await readFrame(this._socket);
    const result = decodeResponse(resp);
    this._state = 'streaming';
    return result;
  }

  /**
   * Send a batch of documents.
   * @param {Object[]} documents - Array of document objects with "id" and field values.
   * @param {{ name: string, type: string }[]} schema - Ordered field definitions.
   * @returns {Promise<{ type: string, batchSeq: bigint, docsReceived: bigint }>}
   */
  async sendBatch(documents, schema) {
    this._ensureConnected();
    if (this._state !== 'streaming') throw new Error(`Cannot send batch: state is ${this._state}, expected streaming`);

    // Encode all documents
    const encodedDocs = [];
    let totalDocBytes = 0;
    for (const doc of documents) {
      const encoded = DocumentEncoder.encode(doc, schema);
      encodedDocs.push(encoded);
      totalDocBytes += encoded.length;
    }

    // DocBatch header: [0x02][u32 doc_count]
    const payload = Buffer.allocUnsafe(1 + 4 + totalDocBytes);
    let pos = 0;
    payload[pos++] = MSG_DOC_BATCH;
    payload.writeUInt32BE(documents.length, pos); pos += 4;
    for (const encoded of encodedDocs) {
      encoded.copy(payload, pos);
      pos += encoded.length;
    }

    await writeFrame(this._socket, payload);
    const resp = await readFrame(this._socket);
    return decodeResponse(resp);
  }

  /**
   * Commit the current ingest session.
   * @returns {Promise<{ type: string, batchSeq: bigint, docsReceived: bigint }>}
   */
  async commit() {
    this._ensureConnected();
    if (this._state !== 'streaming') throw new Error(`Cannot commit: state is ${this._state}, expected streaming`);

    const payload = Buffer.from([MSG_COMMIT]);
    await writeFrame(this._socket, payload);
    const resp = await readFrame(this._socket);
    const result = decodeResponse(resp);
    this._state = 'idle';
    return result;
  }

  /**
   * Close the TCP connection.
   */
  close() {
    if (this._socket) {
      this._socket.destroy();
      this._socket = null;
    }
    this._state = 'idle';
  }

  _ensureConnected() {
    if (!this._socket) throw new Error('Not connected. Call connect() first.');
  }
}

// ── TorqueBinaryWriter ──────────────────────────────────────────────────

export class TorqueBinaryWriter {
  /**
   * Streaming TQBF file writer with zlib compression.
   * @param {{ name: string, type: string }[]} schema - Ordered field definitions.
   * @param {string} filePath - Output file path.
   */
  constructor(schema, filePath) {
    this._schema = schema;
    this._filePath = filePath;
    this._chunks = [];
    this._rawSize = 0;
    this._docCount = 0;
    this._finalized = false;
  }

  /** Number of documents written so far. */
  get documentCount() { return this._docCount; }

  /** Raw (uncompressed) payload size in bytes. */
  get rawSize() { return this._rawSize; }

  /**
   * Write a single document.
   * @param {Object} doc - Document with "id" key and field values.
   */
  write(doc) {
    if (this._finalized) throw new Error('Writer has been finalized');
    const encoded = DocumentEncoder.encode(doc, this._schema);
    this._chunks.push(encoded);
    this._rawSize += encoded.length;
    this._docCount++;
  }

  /**
   * Finalize: compress with zlib and write the TQBF file to disk.
   * @returns {number} Total file size in bytes.
   */
  finalize() {
    if (this._finalized) return 0;
    this._finalized = true;

    const raw = Buffer.concat(this._chunks);
    const uncompressedSize = raw.length;
    const compressed = deflateSync(raw);

    // Header (TQBF v2, 24 bytes):
    //   magic(4) + version(4 LE) + doc_count(4 LE) + flags(4 LE) + uncompressed_size(8 LE)
    const header = Buffer.allocUnsafe(24);
    TQBF_MAGIC.copy(header, 0);
    header.writeUInt32LE(TQBF_VERSION, 4);
    header.writeUInt32LE(this._docCount, 8);
    header.writeUInt32LE(FLAG_ZLIB, 12);
    header.writeBigUInt64LE(BigInt(uncompressedSize), 16);

    const file = Buffer.concat([header, compressed]);
    writeFileSync(this._filePath, file);

    // Free memory
    this._chunks = [];
    return file.length;
  }
}
