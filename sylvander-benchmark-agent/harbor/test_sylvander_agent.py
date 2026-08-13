import unittest

from sylvander_agent import _bounded_diagnostic


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


if __name__ == "__main__":
    unittest.main()
