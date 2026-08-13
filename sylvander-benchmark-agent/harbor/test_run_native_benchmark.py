import importlib.util
import pathlib
import tempfile
import unittest


MODULE_PATH = pathlib.Path(__file__).with_name("run_native_benchmark.py")
SPEC = importlib.util.spec_from_file_location("run_native_benchmark", MODULE_PATH)
assert SPEC is not None and SPEC.loader is not None
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


class ArtifactSecretScanTest(unittest.TestCase):
    def test_redacts_only_the_selected_job_tree(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            log = root / "job.log"
            log.write_text("before secret-value after")
            self.assertEqual(MODULE.redact_and_check(root, "secret-value"), 1)
            self.assertEqual(log.read_text(), "before [REDACTED] after")

    def test_empty_tree_has_no_hits(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            self.assertEqual(MODULE.redact_and_check(pathlib.Path(directory), "key"), 0)


if __name__ == "__main__":
    unittest.main()
