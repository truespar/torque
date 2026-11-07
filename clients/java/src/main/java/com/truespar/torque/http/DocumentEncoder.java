package com.truespar.torque.http;

import java.nio.ByteBuffer;
import java.nio.ByteOrder;
import java.nio.charset.StandardCharsets;
import java.util.ArrayList;
import java.util.LinkedHashMap;
import java.util.LinkedHashSet;
import java.util.List;
import java.util.Map;

/**
 * Encodes {@code Map<String,Object>} documents to binary wire format matching
 * {@code serialize_document} in protocol.rs. All multi-byte values are big-endian.
 *
 * <p>Field types are resolved from the schema ({@code List<Map<String,Object>>}) where each
 * entry has {@code "name"} and {@code "type"} keys. Documents are encoded in schema field order.
 *
 * <p>Type mapping:
 * <ul>
 *   <li>{@code "string"}: u32 BE length + UTF-8 bytes</li>
 *   <li>{@code "int32"}: 4 bytes BE</li>
 *   <li>{@code "int64"}: 8 bytes BE</li>
 *   <li>{@code "float"}: 8 bytes BE (float64)</li>
 *   <li>{@code "bool"}: 1 byte (0 or 1)</li>
 *   <li>{@code "geopoint"}: f64 lat + f64 lng</li>
 *   <li>{@code "string[]"}: u16 count + for each: u8 null flag + string encoding</li>
 *   <li>{@code "int32[]"}: u16 count + for each: u8 null flag + i32 BE</li>
 *   <li>{@code "int64[]"}: u16 count + for each: u8 null flag + i64 BE</li>
 *   <li>{@code "float[]"}: u16 count + for each: f32 BE</li>
 * </ul>
 */
final class DocumentEncoder {

    // ── Language Codes ──────────────────────────────────────────────

    /** Language wire bytes matching the Rust Language enum. */
    static final byte LANG_ENGLISH    = 0;
    static final byte LANG_SWEDISH    = 1;
    static final byte LANG_NORWEGIAN  = 2;
    static final byte LANG_DANISH     = 3;
    static final byte LANG_FINNISH    = 4;
    static final byte LANG_GERMAN     = 5;
    static final byte LANG_FRENCH     = 6;
    static final byte LANG_SPANISH    = 7;
    static final byte LANG_PORTUGUESE = 8;
    static final byte LANG_ITALIAN    = 9;
    static final byte LANG_DUTCH      = 10;
    static final byte LANG_RUSSIAN    = 11;
    static final byte LANG_ARABIC     = 12;
    static final byte LANG_GREEK      = 13;
    static final byte LANG_HUNGARIAN  = 14;
    static final byte LANG_ROMANIAN   = 15;
    static final byte LANG_TURKISH    = 16;

    /** Map ISO 639-1 language codes to wire bytes. */
    static final Map<String, Byte> LANG_MAP = Map.ofEntries(
        Map.entry("en", LANG_ENGLISH),
        Map.entry("sv", LANG_SWEDISH),
        Map.entry("no", LANG_NORWEGIAN),
        Map.entry("da", LANG_DANISH),
        Map.entry("fi", LANG_FINNISH),
        Map.entry("de", LANG_GERMAN),
        Map.entry("fr", LANG_FRENCH),
        Map.entry("es", LANG_SPANISH),
        Map.entry("pt", LANG_PORTUGUESE),
        Map.entry("it", LANG_ITALIAN),
        Map.entry("nl", LANG_DUTCH),
        Map.entry("ru", LANG_RUSSIAN),
        Map.entry("ar", LANG_ARABIC),
        Map.entry("el", LANG_GREEK),
        Map.entry("hu", LANG_HUNGARIAN),
        Map.entry("ro", LANG_ROMANIAN),
        Map.entry("tr", LANG_TURKISH)
    );

    private DocumentEncoder() {}

    // ── Flattener ────────────────────────────────────────────────────

    /**
     * Flatten nested objects/arrays to dotted keys matching the Rust server's
     * {@code flatten_json_object} logic.
     */
    @SuppressWarnings("unchecked")
    static Map<String, Object> flatten(Map<String, Object> doc) {
        return flattenInner(doc, 0);
    }

    @SuppressWarnings("unchecked")
    private static Map<String, Object> flattenInner(Map<String, Object> obj, int depth) {
        var result = new LinkedHashMap<String, Object>();
        for (var entry : obj.entrySet()) {
            String key = entry.getKey();
            Object value = entry.getValue();

            if (value instanceof Map<?, ?> inner) {
                // Nested object: flatten with dotted prefix
                var flatInner = flattenInner((Map<String, Object>) inner, depth + 1);
                for (var ie : flatInner.entrySet()) {
                    result.put(key + "." + ie.getKey(), ie.getValue());
                }
            } else if (value instanceof List<?> arr && !arr.isEmpty() && arr.getFirst() instanceof Map<?, ?>) {
                // Array of objects: pivot to object of arrays
                var seen = new LinkedHashSet<String>();
                for (var item : arr) {
                    if (item instanceof Map<?, ?> itemObj) {
                        for (var k : ((Map<String, Object>) itemObj).keySet()) {
                            seen.add(k);
                        }
                    }
                }
                for (String fieldName : seen) {
                    String arrKey = key + "." + fieldName;
                    var values = new ArrayList<>(arr.size());
                    for (var item : arr) {
                        if (item instanceof Map<?, ?> itemObj) {
                            values.add(((Map<String, Object>) itemObj).get(fieldName));
                        } else {
                            values.add(null);
                        }
                    }
                    result.put(arrKey, values);
                }
            } else {
                result.put(key, value);
            }
        }
        return result;
    }

    // ── Encoder ──────────────────────────────────────────────────────

    /**
     * Encode a single document into the given buffer at the current position.
     *
     * @param doc    document as key-value map; must contain "id" for the external ID
     * @param schema list of field definitions, each with "name" and "type"
     * @param buf    target buffer (big-endian); caller must ensure sufficient capacity
     */
    static void encode(Map<String, Object> doc, List<Map<String, Object>> schema, ByteBuffer buf) {
        doc = flatten(doc);
        // External ID (u16 length prefix)
        String id = String.valueOf(doc.get("id"));
        writeStrU16(buf, id);

        // Language byte (default 0 = English)
        Object langObj = doc.get("language");
        buf.put(langObj instanceof Number n ? n.byteValue() : (byte) 0);

        // Field count (u16)
        buf.putShort((short) schema.size());

        // Fields in schema order
        for (var fieldDef : schema) {
            String name = (String) fieldDef.get("name");
            String type = (String) fieldDef.get("type");
            Object value = doc.get(name);

            if (value == null) {
                buf.put((byte) 0); // null
            } else {
                buf.put((byte) 1); // present
                writeFieldValue(buf, value, type);
            }
        }
    }

    /**
     * Compute exact encoded size of a document for buffer pre-allocation.
     */
    static int computeSize(Map<String, Object> doc, List<Map<String, Object>> schema) {
        doc = flatten(doc);
        String id = String.valueOf(doc.get("id"));
        int size = 2 + utf8Len(id) + 1 + 2; // u16 id_len + id bytes + language + field count

        for (var fieldDef : schema) {
            size += 1; // presence byte
            String name = (String) fieldDef.get("name");
            String type = (String) fieldDef.get("type");
            Object value = doc.get(name);
            if (value != null) {
                size += computeFieldSize(value, type);
            }
        }
        return size;
    }

    private static void writeFieldValue(ByteBuffer buf, Object value, String type) {
        switch (type) {
            case "string" -> writeStr(buf, String.valueOf(value));
            case "int32" -> buf.putInt(toInt(value));
            case "int64" -> buf.putLong(toLong(value));
            case "float" -> buf.putDouble(toDouble(value));
            case "bool" -> buf.put(toBool(value) ? (byte) 1 : (byte) 0);
            case "geopoint" -> writeGeoPoint(buf, value);
            case "string[]" -> writeStringArray(buf, value);
            case "int32[]" -> writeInt32Array(buf, value);
            case "int64[]" -> writeInt64Array(buf, value);
            case "float[]" -> writeFloatArray(buf, value);
            default -> throw new IllegalArgumentException("Unknown field type: " + type);
        }
    }

    private static int computeFieldSize(Object value, String type) {
        return switch (type) {
            case "string" -> 4 + utf8Len(String.valueOf(value));
            case "int32" -> 4;
            case "int64" -> 8;
            case "float" -> 8;
            case "bool" -> 1;
            case "geopoint" -> 16;
            case "string[]" -> computeStringArraySize(value);
            case "int32[]" -> computeInt32ArraySize(value);
            case "int64[]" -> computeInt64ArraySize(value);
            case "float[]" -> computeFloatArraySize(value);
            default -> throw new IllegalArgumentException("Unknown field type: " + type);
        };
    }

    // ── String encoding ───────────────────────────────────────────────

    /** Write string with u32 BE length prefix (matches Rust write_str). */
    private static void writeStr(ByteBuffer buf, String s) {
        byte[] bytes = s.getBytes(StandardCharsets.UTF_8);
        buf.putInt(bytes.length);
        buf.put(bytes);
    }

    /** Write string with u16 BE length prefix (matches Rust write_str_u16). */
    private static void writeStrU16(ByteBuffer buf, String s) {
        byte[] bytes = s.getBytes(StandardCharsets.UTF_8);
        buf.putShort((short) bytes.length);
        buf.put(bytes);
    }

    // ── GeoPoint ──────────────────────────────────────────────────────

    @SuppressWarnings("unchecked")
    private static void writeGeoPoint(ByteBuffer buf, Object value) {
        if (value instanceof List<?> list && list.size() == 2) {
            buf.putDouble(toDouble(list.get(0)));
            buf.putDouble(toDouble(list.get(1)));
        } else if (value instanceof Map<?, ?> map) {
            buf.putDouble(toDouble(((Map<String, Object>) map).get("lat")));
            buf.putDouble(toDouble(((Map<String, Object>) map).get("lng")));
        } else {
            throw new IllegalArgumentException("GeoPoint must be [lat, lng] list or {lat, lng} map");
        }
    }

    // ── Array encodings ───────────────────────────────────────────────

    private static void writeStringArray(ByteBuffer buf, Object value) {
        var list = toList(value);
        buf.putShort((short) list.size());
        for (var item : list) {
            if (item == null) {
                buf.put((byte) 0);
            } else {
                buf.put((byte) 1);
                writeStr(buf, String.valueOf(item));
            }
        }
    }

    private static void writeInt32Array(ByteBuffer buf, Object value) {
        var list = toList(value);
        buf.putShort((short) list.size());
        for (var item : list) {
            if (item == null) {
                buf.put((byte) 0);
            } else {
                buf.put((byte) 1);
                buf.putInt(toInt(item));
            }
        }
    }

    private static void writeInt64Array(ByteBuffer buf, Object value) {
        var list = toList(value);
        buf.putShort((short) list.size());
        for (var item : list) {
            if (item == null) {
                buf.put((byte) 0);
            } else {
                buf.put((byte) 1);
                buf.putLong(toLong(item));
            }
        }
    }

    private static void writeFloatArray(ByteBuffer buf, Object value) {
        var list = toList(value);
        buf.putShort((short) list.size());
        for (var item : list) {
            buf.putFloat(((Number) item).floatValue());
        }
    }

    // ── Array size computation ────────────────────────────────────────

    private static int computeStringArraySize(Object value) {
        var list = toList(value);
        int size = 2; // u16 count
        for (var item : list) {
            size += 1; // null flag
            if (item != null) {
                size += 4 + utf8Len(String.valueOf(item));
            }
        }
        return size;
    }

    private static int computeInt32ArraySize(Object value) {
        var list = toList(value);
        int size = 2;
        for (var item : list) {
            size += 1;
            if (item != null) size += 4;
        }
        return size;
    }

    private static int computeInt64ArraySize(Object value) {
        var list = toList(value);
        int size = 2;
        for (var item : list) {
            size += 1;
            if (item != null) size += 8;
        }
        return size;
    }

    private static int computeFloatArraySize(Object value) {
        var list = toList(value);
        return 2 + list.size() * 4;
    }

    // ── Type conversions ──────────────────────────────────────────────

    private static int toInt(Object v) {
        return v instanceof Number n ? n.intValue() : Integer.parseInt(String.valueOf(v));
    }

    private static long toLong(Object v) {
        return v instanceof Number n ? n.longValue() : Long.parseLong(String.valueOf(v));
    }

    private static double toDouble(Object v) {
        return v instanceof Number n ? n.doubleValue() : Double.parseDouble(String.valueOf(v));
    }

    private static boolean toBool(Object v) {
        return v instanceof Boolean b ? b : Boolean.parseBoolean(String.valueOf(v));
    }

    @SuppressWarnings("unchecked")
    private static List<?> toList(Object v) {
        if (v instanceof List<?> list) return list;
        throw new IllegalArgumentException("Expected list but got: " + v.getClass().getName());
    }

    private static int utf8Len(String s) {
        return s.getBytes(StandardCharsets.UTF_8).length;
    }
}
