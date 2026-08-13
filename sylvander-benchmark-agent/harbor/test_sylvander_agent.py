import unittest

from sylvander_agent import _bounded_diagnostic, _last_json_line, _normalize_machine


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
            '<<<< compose provider exited successfully\n'
        )

        self.assertEqual(metrics["total_prompt_tokens"], 42)
        self.assertEqual(metrics["total_completion_tokens"], 7)

    def test_last_json_line_returns_none_when_runtime_swallows_output(self) -> None:
        self.assertIsNone(_last_json_line(">>>> podman-compose notice\n"))

    def test_machine_aliases_do_not_hide_cross_architecture_execution(self) -> None:
        self.assertEqual(_normalize_machine("arm64\n"), "aarch64")
        self.assertEqual(_normalize_machine("aarch64"), "aarch64")
        self.assertEqual(_normalize_machine("amd64"), "x86_64")
        self.assertEqual(_normalize_machine(">>>> podman notice <<<<\nx86_64\n"), "x86_64")
        self.assertEqual(_normalize_machine("\x1b[0maarch64\n"), "aarch64")
        self.assertNotEqual(_normalize_machine("amd64"), _normalize_machine("arm64"))


if __name__ == "__main__":
    unittest.main()
