import hashlib
import importlib.util
import json
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

    def test_runner_revision_is_bound_to_binary_digest(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            runner = pathlib.Path(directory) / "runner"
            runner.write_bytes(b"binary")
            digest = hashlib.sha256(b"binary").hexdigest()
            pathlib.Path(f"{runner}.json").write_text(json.dumps({
                "sha256": digest, "architecture": "aarch64", "git_commit": "abc123"
            }))
            self.assertEqual(MODULE.runner_revision(runner, digest), "abc123")

    def test_runner_revision_rejects_stale_metadata(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            runner = pathlib.Path(directory) / "runner"
            runner.write_bytes(b"binary")
            pathlib.Path(f"{runner}.json").write_text(json.dumps({
                "sha256": "stale", "architecture": "aarch64", "git_commit": "abc123"
            }))
            with self.assertRaisesRegex(RuntimeError, "does not match"):
                MODULE.runner_revision(runner, hashlib.sha256(b"binary").hexdigest())


if __name__ == "__main__":
    unittest.main()
