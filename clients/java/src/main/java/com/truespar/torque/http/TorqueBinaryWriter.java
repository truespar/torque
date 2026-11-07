package com.truespar.torque.http;

import java.io.*;
import java.nio.ByteBuffer;
import java.nio.ByteOrder;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.List;
import java.util.Map;
import java.util.zip.DeflaterOutputStream;

/**
 * Streaming binary file writer for TQBF format (Torque Binary File).
 * Encodes documents to binary and writes a ZLib-compressed TQBF file on finalize.
 *
 * <p>File format (TQBF v2):
 * <pre>
 *   [4 bytes] magic: "TQBF"
 *   [4 bytes] version: 2 (LE)
 *   [4 bytes] doc_count: u32 (LE)
 *   [4 bytes] flags: 2 = ZLib compressed
 *   [8 bytes] uncompressed_size: u64 (LE) — lets server pre-size decompression buffer
 *   [N bytes] payload: ZLib-compressed concatenated binary documents
 * </pre>
 *
 * <p>Usage:
 * <pre>{@code
 * var writer = new TorqueBinaryWriter(schema, Path.of("output.tqbf"));
 * for (var doc : documents) {
 *     writer.write(doc);
 * }
 * long fileSize = writer.finalizeTqbf();
 * }</pre>
 */
public final class TorqueBinaryWriter {

    private static final int TQBF_VERSION = 2;
    private static final int FLAG_ZLIB = 2;
    private static final int HEADER_SIZE = 24;
    private static final int DEFAULT_ENCODE_BUF_SIZE = 64 * 1024;

    private final List<Map<String, Object>> schema;
    private final Path filePath;
    private final ByteArrayOutputStream rawPayload;
    private byte[] encodeBuf;
    private int docCount;
    private boolean finalized;

    /**
     * Create a binary writer that encodes documents to the TQBF format.
     *
     * @param schema   list of field definitions, each with "name" and "type"
     * @param filePath output file path; written on {@link #finalizeTqbf()}
     */
    public TorqueBinaryWriter(List<Map<String, Object>> schema, Path filePath) {
        this.schema = schema;
        this.filePath = filePath;
        this.rawPayload = new ByteArrayOutputStream();
        this.encodeBuf = new byte[DEFAULT_ENCODE_BUF_SIZE];
    }

    /** Number of documents written so far. */
    public int documentCount() {
        return docCount;
    }

    /** Raw (uncompressed) payload size in bytes. */
    public int rawSize() {
        return rawPayload.size();
    }

    /**
     * Write a single document. The document is encoded to binary and appended
     * to the in-memory payload buffer.
     *
     * @param doc document as key-value map; must contain "id" key
     */
    public void write(Map<String, Object> doc) {
        if (finalized) throw new IllegalStateException("Writer has been finalized.");

        int size = DocumentEncoder.computeSize(doc, schema);
        if (size > encodeBuf.length) {
            encodeBuf = new byte[size];
        }

        var buf = ByteBuffer.wrap(encodeBuf).order(ByteOrder.BIG_ENDIAN);
        DocumentEncoder.encode(doc, schema, buf);
        rawPayload.write(encodeBuf, 0, buf.position());
        docCount++;
    }

    /**
     * Finalize: ZLib-compress the payload and write the TQBF file to disk.
     * Returns the total file size in bytes. Subsequent calls return the existing file size.
     *
     * @return file size in bytes
     * @throws IOException if file writing fails
     */
    public long finalizeTqbf() throws IOException {
        if (finalized) return Files.size(filePath);
        finalized = true;

        byte[] rawBytes = rawPayload.toByteArray();
        long uncompressedSize = rawBytes.length;

        // ZLib compress
        var compressedStream = new ByteArrayOutputStream();
        try (var zlib = new DeflaterOutputStream(compressedStream)) {
            zlib.write(rawBytes);
        }
        byte[] compressed = compressedStream.toByteArray();

        // Write file: header + compressed payload
        try (var fos = new BufferedOutputStream(new FileOutputStream(filePath.toFile()), 65536)) {
            // Header (little-endian, TQBF v2 = 24 bytes)
            var header = ByteBuffer.allocate(HEADER_SIZE).order(ByteOrder.LITTLE_ENDIAN);
            header.put((byte) 'T');
            header.put((byte) 'Q');
            header.put((byte) 'B');
            header.put((byte) 'F');
            header.putInt(TQBF_VERSION);
            header.putInt(docCount);
            header.putInt(FLAG_ZLIB);
            header.putLong(uncompressedSize);
            fos.write(header.array());

            // Compressed payload
            fos.write(compressed);
        }

        return Files.size(filePath);
    }
}
