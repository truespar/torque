package com.truespar.torque.http;

import org.junit.jupiter.api.*;
import java.util.*;

import static org.junit.jupiter.api.Assertions.*;

/**
 * Integration tests for TorqueHttpClient — requires a running Torque server.
 * Set TORQUE_URL and TORQUE_API_KEY env vars or uses localhost defaults.
 */
@TestMethodOrder(MethodOrderer.OrderAnnotation.class)
class TorqueClientTest {

    static final String SERVER = System.getenv().getOrDefault("TORQUE_URL", "http://localhost:8108");
    static final String API_KEY = System.getenv().getOrDefault("TORQUE_API_KEY", "helm-docs");
    static final String COL = "java_integ_test";

    static TorqueHttpClient client;

    @BeforeAll
    static void setup() {
        client = new TorqueHttpClient(SERVER, new TorqueHttpClientOptions(API_KEY));
        cleanup();
    }

    @AfterAll
    static void teardown() {
        cleanup();
        client.close();
    }

    static void cleanup() {
        try { client.deleteCollection(COL); } catch (TorqueApiException ignored) {}
    }

    static Map<String, Object> schema() {
        return Map.of(
            "name", COL,
            "fields", List.of(
                Map.of("name", "title", "type", "string"),
                Map.of("name", "price", "type", "float", "sort", true),
                Map.of("name", "brand", "type", "string", "facet", true),
                Map.of("name", "in_stock", "type", "bool")
            )
        );
    }

    // ── Health ──

    @Test @Order(1)
    void health() {
        var h = client.health();
        assertTrue((Boolean) h.get("ok"));
        assertNotNull(h.get("version"));
    }

    @Test @Order(2)
    void stats() {
        var s = client.stats();
        assertNotNull(s.get("system"));
    }

    // ── Collections ──

    @Test @Order(10)
    void createCollection() {
        var resp = client.createCollection(schema());
        assertEquals(COL, resp.get("name"));
    }

    @Test @Order(11)
    void listCollections() {
        var cols = client.listCollections();
        assertTrue(cols.stream().anyMatch(c -> COL.equals(c.get("name"))));
    }

    @Test @Order(12)
    void getCollection() {
        var col = client.getCollection(COL);
        assertEquals(COL, col.get("name"));
        assertNotNull(col.get("fields"));
    }

    // ── Documents ──

    @Test @Order(20)
    void importDocuments() {
        var jsonl = """
            {"id":"1","title":"Laptop Pro","price":1299.99,"brand":"Acme","in_stock":true}
            {"id":"2","title":"Laptop Basic","price":499.99,"brand":"Acme","in_stock":true}
            {"id":"3","title":"Phone","price":899.99,"brand":"Beta","in_stock":false}
            """;
        var result = client.importDocuments(COL, jsonl, "upsert");
        assertNotNull(result);
        assertTrue(result.contains("success") && !result.contains("\"success\":false"),
            "Import should succeed: " + result.substring(0, Math.min(200, result.length())));
    }

    @Test @Order(21)
    void createDocument() {
        var doc = Map.<String, Object>of(
            "id", "4", "title", "Tablet", "price", 349.99, "brand", "Gamma", "in_stock", true
        );
        var resp = client.createDocument(COL, doc);
        assertEquals("Tablet", resp.get("title"));
    }

    @Test @Order(22)
    void getDocument() {
        var doc = client.getDocument(COL, "1");
        assertEquals("Laptop Pro", doc.get("title"));
    }

    @Test @Order(23)
    void updateDocument() {
        var resp = client.updateDocument(COL, "1", Map.of("price", 1199.99));
        assertNotNull(resp);
    }

    @Test @Order(24)
    void deleteDocument() {
        client.deleteDocument(COL, "4");
        assertThrows(TorqueApiException.class, () -> client.getDocument(COL, "4"));
    }

    // ── Search ──

    @Test @Order(30)
    void search() {
        var sr = client.search(COL, Map.of("q", "laptop", "query_by", "title", "per_page", "10"));
        assertTrue(((Number) sr.get("found")).intValue() >= 2);
        assertNotNull(sr.get("hits"));
    }

    @Test @Order(31)
    void searchWithFilter() {
        var sr = client.search(COL, Map.of("q", "*", "query_by", "title", "filter_by", "brand:=Acme"));
        assertEquals(2, ((Number) sr.get("found")).intValue());
    }

    @Test @Order(32)
    void searchWithFacet() {
        var sr = client.search(COL, Map.of("q", "*", "query_by", "title", "facet_by", "brand"));
        assertTrue(((Number) sr.get("found")).intValue() >= 3);
        assertNotNull(sr.get("facet_counts"));
    }

    @Test @Order(33)
    void multiSearch() {
        var searches = List.<Map<String, Object>>of(
            Map.of("collection", COL, "q", "laptop", "query_by", "title"),
            Map.of("collection", COL, "q", "phone", "query_by", "title")
        );
        var resp = client.multiSearch(searches);
        assertNotNull(resp.get("results"));
        @SuppressWarnings("unchecked")
        var results = (List<Map<String, Object>>) resp.get("results");
        assertEquals(2, results.size());
    }

    // ── Aliases ──

    @Test @Order(40)
    void aliasCrud() {
        var alias = client.upsertAlias("java_alias", COL);
        assertEquals(COL, alias.get("collection_name"));

        var got = client.getAlias("java_alias");
        assertEquals(COL, got.get("collection_name"));

        var list = client.listAliases();
        assertNotNull(list.get("aliases"));

        client.deleteAlias("java_alias");
    }

    // ── Presets ──

    @Test @Order(50)
    void presetCrud() {
        var preset = client.upsertPreset("java_preset", Map.of(
            "id", "java_preset", "value", Map.of("per_page", 20)
        ));
        assertNotNull(preset);

        var list = client.listPresets();
        assertNotNull(list.get("presets"));

        client.deletePreset("java_preset");
    }

    // ── Stopwords ──

    @Test @Order(60)
    void stopwordCrud() {
        var sw = client.upsertStopwords("java_sw", Map.of(
            "id", "java_sw", "stopwords", List.of("the", "a")
        ));
        assertNotNull(sw);

        var list = client.listStopwords();
        assertNotNull(list.get("stopwords"));

        client.deleteStopwords("java_sw");
    }

    // ── TCP Binary Ingest ──

    @Test @Order(70)
    void tcpIngest() throws Exception {
        cleanup();
        client.createCollection(schema());

        var fields = List.<Map<String, Object>>of(
            Map.of("name", "title", "type", "string"),
            Map.of("name", "price", "type", "float"),
            Map.of("name", "brand", "type", "string"),
            Map.of("name", "in_stock", "type", "bool")
        );
        var docs = List.<Map<String, Object>>of(
            Map.of("id", "1", "title", "Laptop", "price", 999.99, "brand", "Acme", "in_stock", true),
            Map.of("id", "2", "title", "Phone", "price", 499.99, "brand", "Beta", "in_stock", false)
        );

        try (var tcp = new TorqueIngestClient()) {
            tcp.connect("localhost", 8119);
            tcp.startIngest(COL);
            var ack = tcp.sendBatch(docs, fields);
            assertEquals(2L, ack.docsReceived());
            var commitAck = tcp.commit();
            assertEquals(2L, commitAck.docsReceived());
        }

        Thread.sleep(5000); // wait for async index build

        var sr = client.search(COL, Map.of("q", "laptop", "query_by", "title"));
        assertTrue(((Number) sr.get("found")).intValue() >= 1);
    }

    // ── Delete collection (last) ──

    @Test @Order(99)
    void deleteCollection() {
        client.deleteCollection(COL);
        assertThrows(TorqueApiException.class, () -> client.getCollection(COL));
    }
}
