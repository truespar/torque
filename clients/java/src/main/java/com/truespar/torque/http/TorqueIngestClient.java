package com.truespar.torque.http;

import java.io.DataInputStream;
import java.io.DataOutputStream;
import java.io.EOFException;
import java.io.IOException;
import java.net.Socket;
import java.nio.ByteBuffer;
import java.nio.ByteOrder;
import java.nio.charset.StandardCharsets;
import java.util.List;
import java.util.Map;

/**
 * TCP ingest client for streaming documents to Torque via the binary protocol.
 * Implements {@link AutoCloseable} for resource cleanup.
 *
 * <p>Frame format: {@code [u32 BE: payload_length][payload_bytes...]}
 *
 * <p>Usage:
 * <pre>{@code
 * try (var ingest = new TorqueIngestClient()) {
 *     ingest.connect("localhost", 8119);
 *     ingest.startIngest("my-collection");
 *     ingest.sendBatch(documents, schema);
 *     ingest.commit();
 * }
 * }</pre>
 */
public final class TorqueIngestClient implements AutoCloseable {

    // Client -> Server message types
    private static final byte MSG_START_INGEST = 0x01;
    private static final byte MSG_DOC_BATCH = 0x02;
    private static final byte MSG_COMMIT = 0x03;
    private static final byte MSG_ABORT = 0x04;

    // Server -> Client message types
    private static final byte MSG_ACK = 0x10;
    private static final byte MSG_COMMIT_RESULT = 0x11;
    private static final byte MSG_ERROR = 0x12;

    private static final int INITIAL_SEND_BUF_SIZE = 64 * 1024;
    private static final int RECV_BUF_SIZE = 4 * 1024;

    private Socket socket;
    private DataOutputStream out;
    private DataInputStream in;
    private byte[] sendBuf = new byte[INITIAL_SEND_BUF_SIZE];
    private final byte[] recvBuf = new byte[RECV_BUF_SIZE];

    /**
     * Connect to the Torque TCP ingest server.
     *
     * @param host server hostname
     * @param port server port (typically 8119)
     * @throws IOException if the connection fails
     */
    public void connect(String host, int port) throws IOException {
        if (socket != null) throw new IllegalStateException("Already connected");
        socket = new Socket(host, port);
        socket.setTcpNoDelay(true);
        out = new DataOutputStream(socket.getOutputStream());
        in = new DataInputStream(socket.getInputStream());
    }

    /**
     * Start an ingest session for the given collection. Sends a StartIngest message
     * and waits for server acknowledgement.
     *
     * @param collection collection name
     * @return server ack with batch sequence and docs received
     * @throws IOException       on I/O error
     * @throws TorqueApiException if the server returns an error
     */
    public Ack startIngest(String collection) throws IOException {
        ensureConnected();
        byte[] collBytes = collection.getBytes(StandardCharsets.UTF_8);

        // StartIngest: [0x01][u32 coll_len][coll_bytes][u8 mode=0]
        int msgSize = 1 + 4 + collBytes.length + 1;
        ensureSendCapacity(4 + msgSize);

        var buf = ByteBuffer.wrap(sendBuf).order(ByteOrder.BIG_ENDIAN);
        buf.putInt(msgSize);        // frame length prefix
        buf.put(MSG_START_INGEST);
        buf.putInt(collBytes.length);
        buf.put(collBytes);
        buf.put((byte) 0);         // mode = batch

        writeFrame(buf.position());
        return readAck();
    }

    /**
     * Send a batch of documents encoded in binary format. Each batch is a single TCP frame.
     *
     * @param documents list of document maps (must contain "id" key)
     * @param schema    list of field definitions, each with "name" and "type"
     * @return server ack with cumulative document count
     * @throws IOException       on I/O error
     * @throws TorqueApiException if the server returns an error
     */
    public Ack sendBatch(List<Map<String, Object>> documents, List<Map<String, Object>> schema) throws IOException {
        ensureConnected();
        if (documents.isEmpty()) throw new IllegalArgumentException("No documents to send");

        // Pre-compute total size: frame header + msg type + doc count + all docs
        int totalSize = 4 + 1 + 4;
        for (var doc : documents) {
            totalSize += DocumentEncoder.computeSize(doc, schema);
        }
        ensureSendCapacity(totalSize);

        var buf = ByteBuffer.wrap(sendBuf).order(ByteOrder.BIG_ENDIAN);
        buf.putInt(totalSize - 4);  // frame length prefix (excludes itself)
        buf.put(MSG_DOC_BATCH);
        buf.putInt(documents.size());

        for (var doc : documents) {
            DocumentEncoder.encode(doc, schema, buf);
        }

        writeFrame(buf.position());
        return readAck();
    }

    /**
     * Commit the current ingest session. The server will build the index asynchronously.
     *
     * @return server ack
     * @throws IOException       on I/O error
     * @throws TorqueApiException if the server returns an error
     */
    public Ack commit() throws IOException {
        ensureConnected();
        var buf = ByteBuffer.wrap(sendBuf).order(ByteOrder.BIG_ENDIAN);
        buf.putInt(1);          // frame length: 1 byte payload
        buf.put(MSG_COMMIT);
        writeFrame(buf.position());
        return readAck();
    }

    /**
     * Abort the current ingest session, discarding all accumulated documents.
     *
     * @return server ack
     * @throws IOException       on I/O error
     * @throws TorqueApiException if the server returns an error
     */
    public Ack abort() throws IOException {
        ensureConnected();
        var buf = ByteBuffer.wrap(sendBuf).order(ByteOrder.BIG_ENDIAN);
        buf.putInt(1);
        buf.put(MSG_ABORT);
        writeFrame(buf.position());
        return readAck();
    }

    @Override
    public void close() throws IOException {
        if (socket != null) {
            socket.close();
            socket = null;
            out = null;
            in = null;
        }
    }

    // ── Frame I/O ─────────────────────────────────────────────────────

    private void writeFrame(int totalLength) throws IOException {
        out.write(sendBuf, 0, totalLength);
        out.flush();
    }

    private int readFrame() throws IOException {
        in.readFully(recvBuf, 0, 4);
        int length = ByteBuffer.wrap(recvBuf, 0, 4).order(ByteOrder.BIG_ENDIAN).getInt();
        if (length < 0 || length > recvBuf.length) {
            throw new TorqueApiException("Response frame too large: " + Integer.toUnsignedString(length)
                    + " bytes exceeds buffer size " + recvBuf.length);
        }
        if (length > 0) {
            in.readFully(recvBuf, 0, length);
        }
        return length;
    }

    // ── Response decoding ─────────────────────────────────────────────

    private Ack readAck() throws IOException {
        int length = readFrame();
        if (length == 0) throw new TorqueApiException("Empty server response");

        byte msgType = recvBuf[0];
        return switch (msgType) {
            case MSG_ACK -> decodeAck(length);
            case MSG_COMMIT_RESULT -> decodeAck(length);
            case MSG_ERROR -> throw decodeError(length);
            default -> throw new TorqueApiException("Unknown server message type: 0x"
                    + Integer.toHexString(msgType & 0xFF));
        };
    }

    private Ack decodeAck(int length) {
        // [u8 type][u64 batch_seq][u64 docs_received]
        if (length < 17) throw new TorqueApiException("Ack message too short: " + length);
        var buf = ByteBuffer.wrap(recvBuf, 1, length - 1).order(ByteOrder.BIG_ENDIAN);
        return new Ack(buf.getLong(), buf.getLong());
    }

    private TorqueApiException decodeError(int length) {
        // [u8 type][u16 code][u32 msg_len][UTF-8 message]
        if (length < 7) return new TorqueApiException("Server error (message too short)");
        var buf = ByteBuffer.wrap(recvBuf, 1, length - 1).order(ByteOrder.BIG_ENDIAN);
        int code = Short.toUnsignedInt(buf.getShort());
        int msgLen = buf.getInt();
        String message = msgLen > 0 && msgLen <= buf.remaining()
                ? new String(recvBuf, 7, msgLen, StandardCharsets.UTF_8)
                : "Unknown error";
        return new TorqueApiException("Server error " + code + ": " + message);
    }

    // ── Helpers ───────────────────────────────────────────────────────

    private void ensureConnected() {
        if (socket == null) throw new IllegalStateException("Not connected. Call connect() first.");
    }

    private void ensureSendCapacity(int needed) {
        if (needed <= sendBuf.length) return;
        int newSize = Math.max(sendBuf.length * 2, needed);
        byte[] newBuf = new byte[newSize];
        System.arraycopy(sendBuf, 0, newBuf, 0, sendBuf.length);
        sendBuf = newBuf;
    }

    /**
     * Server acknowledgement containing batch sequence number and cumulative document count.
     */
    public record Ack(long batchSeq, long docsReceived) {}
}
