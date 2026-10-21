import argparse
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

import compare
import audit
import native
import report


class ComparisonTests(unittest.TestCase):
    def test_committed_local_branches_ignore_dirty_checkout(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            subprocess.run(["git", "init", "-q", root], check=True)
            (root / "data").write_text("committed")
            subprocess.run(["git", "-C", root, "add", "data"], check=True)
            subprocess.run(["git", "-C", root, "-c", "user.name=Test", "-c",
                            "user.email=test@example.com", "-c", "commit.gpgsign=false",
                            "commit", "-qm", "initial"], check=True)
            for name in ("upstream", "river-raptor"):
                compare.git(root, "branch", name)
            compare.git(root, "remote", "add", "upstream", "https://example.invalid/repo")
            (root / "data").write_text("dirty")
            refs = compare.resolve_branches(root)
            self.assertEqual(refs["upstream"]["commit"], compare.git(root, "rev-parse", "HEAD"))
            self.assertEqual(refs["river-raptor"]["ref"], "refs/heads/river-raptor")
            self.assertEqual((root / "data").read_text(), "dirty")

    def test_patch_application_records_hash_and_fails_closed(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            tree, evidence = root / "tree", root / "sources/engine"
            tree.mkdir()
            evidence.mkdir(parents=True)
            (tree / "file").write_text("before\n")
            adapter = root / "adapter.patch"
            adapter.write_text("diff --git a/file b/file\n--- a/file\n+++ b/file\n"
                               "@@ -1 +1 @@\n-before\n+after\n")
            first = compare.apply_adapter(tree, adapter, evidence)
            second = compare.apply_adapter(tree, adapter, evidence)
            self.assertEqual(first["status"], "applied")
            self.assertEqual(second["status"], "already-present")
            self.assertEqual(first["sha256"], compare.digest(adapter.read_bytes()))
            (tree / "file").write_text("unexpected\n")
            with self.assertRaisesRegex(RuntimeError, "no longer matches"):
                compare.apply_adapter(tree, adapter, evidence)

    def test_run_directories_preserve_previous_failures(self):
        with tempfile.TemporaryDirectory() as tmp:
            first = compare.new_run(Path(tmp))
            (first / "failure.log").write_text("retain me")
            second = compare.new_run(Path(tmp))
            self.assertNotEqual(first, second)
            self.assertEqual((first / "failure.log").read_text(), "retain me")

    def test_html_shows_human_fields_without_json_dump(self):
        value = {"id": "<unsafe>", "status": "failed", "error": "</script><script>bad()",
                 "results": [], "engines": {}, "arbitrary_data": [1, 2, 3]}
        page = report.render(value)
        self.assertIn("&lt;unsafe&gt;", page)
        self.assertNotIn("</script><script>bad()", page)
        self.assertNotIn('"arbitrary_data": [1, 2, 3]', page)
        self.assertNotIn('type="application/json"', page)
        self.assertNotIn('id="raw"', page)
        self.assertIn("Timing comparisons", page)
        self.assertIn("Reproduction details", page)
        self.assertIn("report.json", page)

    def test_html_exposes_measurements_ranges_and_sample_details(self):
        case = next(native.cases(Path("short"), Path("long")))
        samples = [{"ns_per_operation": 2000, "step_ns": 2000, "wall_ns": 20000},
                   {"ns_per_operation": 4000, "step_ns": 4000, "wall_ns": 40000}]
        case.update(samples={e: samples for e in report.ENGINES},
                    median_ns={e: 3000 for e in report.ENGINES})
        page = report.render({"id": "test", "status": "complete", "results": [case], "engines": {}})
        self.assertIn("3.00 µs", page)
        self.assertIn("2.00 µs–4.00 µs · n=2", page)
        self.assertIn("2,000.0</b> ns/op", page)
        self.assertIn("4,000.0</b> ns/op", page)
        self.assertIn('class="bar ghostty"', page)
        self.assertIn("Samples and setup", page)

    def test_bars_use_river_reference_and_display_relative_time(self):
        case = next(native.cases(Path("short"), Path("long")))
        case.update(samples={e: [] for e in report.ENGINES},
                    median_ns={"river-raptor": 100, "upstream": 120, "ghostty": 80})
        page = report.render({"id": "test", "status": "complete", "results": [case], "engines": {}})
        self.assertIn('class="bar river-raptor" style="width:50.00%"', page)
        self.assertIn('class="bar upstream" style="width:60.00%"', page)
        self.assertIn('class="bar ghostty" style="width:40.00%"', page)
        self.assertIn("+20.0% time", page)
        self.assertIn("−20.0% time", page)
        self.assertIn('<th>River-raptor</th><th>Upstream</th><th>Ghostty</th>', page)

    def test_clone_cost_overflow_is_not_presented_as_equivalent_speedup(self):
        case = next(c for c in native.cases(Path("short"), Path("long")) if c["case"] == "screen-clone/clone")
        case.update(samples={e: [] for e in report.ENGINES},
                    median_ns={"river-raptor": 100, "upstream": 100, "ghostty": 10000})
        page = report.render({"id": "test", "status": "complete", "results": [case], "engines": {}})
        self.assertIn('class="bar ghostty" style="width:100.00%"', page)
        self.assertIn("+9,900.0% time", page)
        self.assertIn('class="overflow"', page)
        self.assertIn("Different work", page)
        self.assertIn("100.00× native cost ratio", page)

    def test_case_normalization_and_corpus_selection(self):
        cases = {c["case"]: c for c in native.cases(Path("short"), Path("long"))}
        self.assertEqual(len(cases), 53)
        self.assertEqual(cases["screen-clone/render-clean"]["operations"], 3000000)
        self.assertEqual(cases["terminal-resize/rows"]["operations"], 2500000)
        self.assertIn("long/ascii.bin", cases["terminal-stream/ascii"]["rio_args"])
        self.assertIn("short/osc8.bin", cases["screen-clone/clone"]["rio_args"])
        self.assertTrue(cases["terminal-formatter/vt/noop"]["control"])
        self.assertNotIn("--data", cases["hyperlink-map/lookup/25"]["rio_args"])

    def test_outlier_selection_excludes_controls_and_failed_cases(self):
        def case(name, ratio, **extra):
            return dict(case=name, control=False, median_ns={"river-raptor": 100, "ghostty": 100 * ratio}, **extra)
        control = case("control", 100)
        control["control"] = True
        selected = audit.outliers({"results": [case("large", 12), case("slow", .05),
                                               case("small", 3), control, case("failed", 50, error="bad")]}, 10)
        self.assertEqual([c["case"] for c in selected], ["large", "slow"])

    def test_audit_rejects_a_changed_saved_binary(self):
        with tempfile.TemporaryDirectory() as tmp:
            binary = Path(tmp) / "binary"
            binary.write_bytes(b"original")
            saved = {"engines": {"ghostty": {"binary": {"path": str(binary),
                     "sha256": compare.digest(binary.read_bytes())}}}, "results": []}
            audit.validate_inputs(saved)
            binary.write_bytes(b"different")
            with self.assertRaisesRegex(ValueError, "Saved binary hash differs"):
                audit.validate_inputs(saved)

    def test_resize_setup_mismatch_invalidates_comparison(self):
        with tempfile.TemporaryDirectory() as tmp:
            directory = Path(tmp)
            case = next(c for c in native.cases(directory, directory) if c["case"] == "terminal-resize/cols")
            value = {"id": "test", "status": "running", "cpu": None, "require_matched_resize_setup": True,
                     "engines": {e: {"commit": "abc", "binary": {"path": "/unused"}} for e in report.ENGINES},
                     "results": [], "failures": []}
            def sample(history):
                return {"ns_per_operation": 10, "rio_report": {"rio_revision": "abc"},
                        "setup": {"columns": 120, "rows": 80, "history_lines": history}}
            with patch.object(compare, "sample", side_effect=[sample(10000), sample(10000), sample(20000)]):
                compare.measure(argparse.Namespace(samples=1, warmups=0, timeout=1), directory, value, [case])
            self.assertIn("Resize setup differs", value["failures"][0]["error"])
            self.assertNotIn("median_ns", value["results"][0])

    def test_failure_saves_partial_samples_and_continues(self):
        with tempfile.TemporaryDirectory() as tmp:
            directory = Path(tmp)
            case = next(native.cases(directory, directory))
            value = {"id": "test", "status": "running", "cpu": None,
                     "engines": {e: {"commit": "abc", "binary": {"path": "/unused"}} for e in report.ENGINES},
                     "results": [], "failures": []}
            good = {"ns_per_operation": 10, "rio_report": {"checksum": "stable", "rio_revision": "abc"}}
            with patch.object(compare, "sample", side_effect=[good, RuntimeError("sample failure")]):
                compare.measure(argparse.Namespace(samples=1, warmups=0, timeout=1),
                                directory, value, [case])
            saved = json.loads((directory / "report.json").read_text())
            self.assertEqual(len(saved["results"][0]["samples"]["upstream"]), 1)
            self.assertEqual(saved["failures"][0]["error"], "sample failure")
            self.assertTrue((directory / "report.html").exists())


if __name__ == "__main__":
    unittest.main()
