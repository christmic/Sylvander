import unittest

from sylvander_agent import _bounded_diagnostic, _last_json_line


class BoundedDiagnosticTest(unittest.TestCase):
    def test_redacts_the_active_api_key(self) -> None:
        diagnostic = _bounded_diagnostic(
            "request failed with secret-value", "provider output", "secret-value"
        )
        self.assertEqual(
            diagnostic, "request failed with [REDACTED]\nprovider output"
        )

    def test_bounds_persisted_output(self) -> None:
        diagnostic = _bounded_diagnostic("x" * 2_100, None, "unused-secret")
        self.assertEqual(len(diagnostic), 2_001)
        self.assertTrue(diagnostic.endswith("…"))

    def test_last_json_line_ignores_runtime_notice(self) -> None:
        metrics = _last_json_line(
            '>>>> Executing external compose provider "podman-compose".\n'
            '{"total_prompt_tokens": 42, "total_completion_tokens": 7}\n'
        )

        self.assertEqual(metrics["total_prompt_tokens"], 42)
        self.assertEqual(metrics["total_completion_tokens"], 7)


if __name__ == "__main__":
    unittest.main()
