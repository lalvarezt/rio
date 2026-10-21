import copy
import importlib.util
import json
import tempfile
import unittest
from argparse import Namespace
from pathlib import Path

import paired

spec = importlib.util.spec_from_file_location(
    "suite", Path(__file__).with_name("suite.py")
)
suite = importlib.util.module_from_spec(spec)
spec.loader.exec_module(suite)


def result():
    return {
        "case": "test",
        "suite": "ghostty-bench",
        "version": 1,
        "optimized_build": True,
        "ghostty_revision": suite.GHOSTTY_REVISION,
        "options": {
            "action": "terminal-parser",
            "mode": "default",
            "emit": "vt",
            "io": "file",
            "data": "a.bin",
            "cols": 80,
            "rows": 24,
            "resize_cols": 40,
            "resize_rows": 12,
            "entries": 4096,
            "chunk_size": 65536,
            "loops": 1,
            "samples": 2,
            "warmups": 1,
            "fill_lines": 100,
            "scrollback": 100,
            "load_percent": 100,
        },
        "checksum": "a" * 16,
        "sample_ns": [100, 100],
        "median_ns_per_operation": 100,
    }


def capture():
    return {
        "suite": "ghostty-bench",
        "version": 1,
        "corpora": {
            "version": 1,
            "files": {"ascii.bin": {"bytes": 10, "sha256": "a" * 64}},
        },
        "results": [result()],
    }


class SuiteTests(unittest.TestCase):
    def test_paired_aggregation_keeps_common_operations_and_raw_samples(self):
        first = result()
        first["options"]["samples"] = 1
        first["sample_ns"] = [100]
        second = copy.deepcopy(first)
        second["sample_ns"] = [200]
        second["median_ns_per_operation"] = 200
        combined = paired.aggregate([first, second], 2)
        self.assertEqual(combined["sample_ns"], [100, 200])
        self.assertEqual(combined["median_ns_per_operation"], 150)
        self.assertEqual(combined["options"]["samples"], 2)
        second["checksum"] = "b" * 16
        with self.assertRaises(ValueError):
            paired.aggregate([first, second], 2)

    def test_different_output_can_be_reported_without_a_timing_ratio(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            before = capture()
            after = copy.deepcopy(before)
            after["results"][0]["checksum"] = "b" * 16
            args = Namespace(
                before=directory / "a.json",
                after=directory / "b.json",
                output=directory / "c.json",
                report_output_differences=True,
            )
            args.before.write_text(json.dumps(before))
            args.after.write_text(json.dumps(after))
            suite.compare(args)
            comparison = json.loads(args.output.read_text())[0]
            self.assertFalse(comparison["comparable"])
            self.assertNotIn("after_over_before", comparison)

    def test_seeded_corpora_are_repeatable_and_records_are_complete(self):
        with tempfile.TemporaryDirectory() as temporary:
            first, second = Path(temporary) / "a", Path(temporary) / "b"
            for output in (first, second):
                suite.generate(Namespace(output=output, bytes=1024, seed=42))
            self.assertEqual(
                (first / "manifest.json").read_bytes(),
                (second / "manifest.json").read_bytes(),
            )
            manifest = json.loads((first / "manifest.json").read_text())
            for filename, info in manifest["files"].items():
                data = (first / filename).read_bytes()
                self.assertEqual(data, (second / filename).read_bytes())
                self.assertEqual(info["sha256"], suite.sha256(data))
            data = (first / "osc-records.bin").read_bytes()
            while data:
                size = int.from_bytes(data[:8], "little")
                self.assertLessEqual(size, 4096)
                self.assertGreaterEqual(len(data), 8 + size)
                data = data[8 + size :]

    def test_existing_corpus_cannot_be_changed_by_another_seed(self):
        with tempfile.TemporaryDirectory() as temporary:
            output = Path(temporary)
            suite.generate(Namespace(output=output, bytes=1024, seed=42))
            previous = (output / "ascii.bin").read_bytes()
            with self.assertRaises(ValueError):
                suite.generate(Namespace(output=output, bytes=1024, seed=43))
            self.assertEqual(previous, (output / "ascii.bin").read_bytes())

    def test_compare_rejects_different_outputs_or_parameters(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            before = capture()
            after = copy.deepcopy(before)
            after["results"][0]["sample_ns"] = [90, 90]
            after["results"][0]["median_ns_per_operation"] = 90
            args = Namespace(
                before=directory / "before.json",
                after=directory / "after.json",
                output=directory / "comparison.json",
            )
            args.before.write_text(json.dumps(before))
            args.after.write_text(json.dumps(after))
            suite.compare(args)
            self.assertEqual(
                json.loads(args.output.read_text())[0]["after_over_before"], 0.9
            )
            for change in ({"checksum": "bbb"}, {"options": {"cols": 120}}):
                after["results"][0].update(change)
                args.after.write_text(json.dumps(after))
                with self.assertRaises(ValueError):
                    suite.compare(args)

    def test_invalid_timings_and_build_modes_are_rejected(self):
        for update in (
            {"median_ns_per_operation": float("nan")},
            {"median_ns_per_operation": float("inf")},
            {"median_ns_per_operation": 0},
            {"sample_ns": [0, 100]},
            {"sample_ns": [-1, 201]},
            {"version": 2},
            {"suite": "other"},
            {"optimized_build": "true"},
        ):
            value = result()
            value.update(update)
            with self.assertRaises(ValueError):
                suite.validate_result(value)
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            before = capture()
            after = copy.deepcopy(before)
            after["results"][0]["optimized_build"] = False
            args = Namespace(
                before=directory / "a.json",
                after=directory / "b.json",
                output=directory / "c.json",
            )
            args.before.write_text(json.dumps(before))
            args.after.write_text(json.dumps(after))
            with self.assertRaisesRegex(ValueError, "build modes"):
                suite.compare(args)

    def test_selected_corpus_must_be_in_the_verified_manifest(self):
        with tempfile.TemporaryDirectory() as temporary:
            output = Path(temporary)
            suite.generate(Namespace(output=output, bytes=1024, seed=42))
            manifest_path = output / "manifest.json"
            manifest = json.loads(manifest_path.read_text())
            del manifest["files"]["ascii.bin"]
            manifest_path.write_text(json.dumps(manifest))
            with self.assertRaisesRegex(ValueError, "selected corpus missing"):
                suite.capture(
                    Namespace(
                        data_dir=output,
                        filter="terminal-parser/ascii",
                        binary=output / "unused",
                        output=output / "capture.json",
                    )
                )


if __name__ == "__main__":
    unittest.main()
