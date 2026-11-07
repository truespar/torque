"""Integration tests for TorqueHttpClient — requires a running Torque server."""

import os
import sys
import unittest

sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "src"))
from torque_http import TorqueHttpClient, TorqueApiException, CollectionSchema, FieldDefinition


SERVER = os.environ.get("TORQUE_URL", "http://localhost:8108")
API_KEY = os.environ.get("TORQUE_API_KEY", "helm-docs")


class TestTorqueHttpClient(unittest.TestCase):
    """Integration tests — skipped if server is not reachable."""

    @classmethod
    def setUpClass(cls):
        cls.client = TorqueHttpClient(SERVER, api_key=API_KEY)
        try:
            cls.client.health()
        except Exception:
            raise unittest.SkipTest("Torque server not reachable")

    def setUp(self):
        """Clean up test collection before each test."""
        try:
            self.client.delete_collection("py_test")
        except TorqueApiException:
            pass

    def tearDown(self):
        try:
            self.client.delete_collection("py_test")
        except TorqueApiException:
            pass

    def _create_test_collection(self):
        schema = CollectionSchema(
            name="py_test",
            fields=[
                FieldDefinition(name="title", type="string"),
                FieldDefinition(name="price", type="float", sort=True),
                FieldDefinition(name="brand", type="string", facet=True),
            ],
        )
        return self.client.create_collection(schema)

    # ── Health ──

    def test_health(self):
        h = self.client.health()
        self.assertTrue(h.ok)
        self.assertIn("0.", h.version)

    def test_stats(self):
        s = self.client.stats()
        self.assertIn("memory", s.system)

    # ── Collections ──

    def test_create_and_list_collection(self):
        resp = self._create_test_collection()
        self.assertEqual(resp.name, "py_test")

        cols = self.client.list_collections()
        names = [c.name for c in cols]
        self.assertIn("py_test", names)

    def test_get_collection(self):
        self._create_test_collection()
        resp = self.client.get_collection("py_test")
        self.assertEqual(resp.name, "py_test")
        self.assertTrue(len(resp.fields) >= 3)

    def test_delete_collection(self):
        self._create_test_collection()
        self.client.delete_collection("py_test")
        with self.assertRaises(TorqueApiException) as ctx:
            self.client.get_collection("py_test")
        self.assertEqual(ctx.exception.status_code, 404)

    # ── Documents ──

    def test_create_and_get_document(self):
        self._create_test_collection()
        doc = {"id": "1", "title": "Laptop", "price": 999.99, "brand": "Acme"}
        self.client.create_document("py_test", doc)

        result = self.client.get_document("py_test", "1")
        self.assertEqual(result["title"], "Laptop")
        self.assertEqual(result["price"], 999.99)

    def test_import_documents(self):
        self._create_test_collection()
        docs = [
            {"id": "1", "title": "Laptop", "price": 999.99, "brand": "Acme"},
            {"id": "2", "title": "Phone", "price": 499.99, "brand": "Beta"},
        ]
        results = self.client.import_documents("py_test", docs)
        self.assertEqual(len(results), 2)
        self.assertTrue(all(r.success for r in results))

    def test_delete_document(self):
        self._create_test_collection()
        self.client.create_document("py_test", {"id": "1", "title": "Laptop", "price": 999.99, "brand": "Acme"})
        self.client.delete_document("py_test", "1")
        with self.assertRaises(TorqueApiException) as ctx:
            self.client.get_document("py_test", "1")
        self.assertEqual(ctx.exception.status_code, 404)

    # ── Search ──

    def test_search(self):
        self._create_test_collection()
        self.client.import_documents("py_test", [
            {"id": "1", "title": "Laptop Pro", "price": 1299.99, "brand": "Acme"},
            {"id": "2", "title": "Laptop Basic", "price": 499.99, "brand": "Acme"},
            {"id": "3", "title": "Phone", "price": 899.99, "brand": "Beta"},
        ])
        result = self.client.search("py_test", q="laptop", query_by="title", per_page=10)
        self.assertGreater(result.found, 0)
        self.assertTrue(len(result.hits) > 0)

    def test_search_with_filter(self):
        self._create_test_collection()
        self.client.import_documents("py_test", [
            {"id": "1", "title": "Laptop", "price": 999.99, "brand": "Acme"},
            {"id": "2", "title": "Phone", "price": 499.99, "brand": "Beta"},
        ])
        result = self.client.search("py_test", q="*", query_by="title", filter_by="brand:=Acme")
        self.assertEqual(result.found, 1)

    # ── Aliases ──

    def test_alias_crud(self):
        self._create_test_collection()
        alias = self.client.upsert_alias("py_alias", "py_test")
        self.assertEqual(alias.collection_name, "py_test")

        got = self.client.get_alias("py_alias")
        self.assertEqual(got.collection_name, "py_test")

        aliases = self.client.list_aliases()
        self.assertTrue(any(a.name == "py_alias" for a in aliases))

        self.client.delete_alias("py_alias")

    # ── Multi-search ──

    def test_multi_search(self):
        self._create_test_collection()
        self.client.import_documents("py_test", [
            {"id": "1", "title": "Laptop", "price": 999.99, "brand": "Acme"},
        ])
        results = self.client.multi_search([
            {"collection": "py_test", "q": "laptop", "query_by": "title"},
            {"collection": "py_test", "q": "*", "query_by": "title"},
        ])
        self.assertEqual(len(results), 2)


if __name__ == "__main__":
    unittest.main()
