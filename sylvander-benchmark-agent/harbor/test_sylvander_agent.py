import unittest
from pathlib import Path
from unittest.mock import AsyncMock

from sylvander_agent import SylvanderAgent, _bounded_diagnostic, _last_json_line, _normalize_machine


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


class CredentialTransportTest(unittest.IsolatedAsyncioTestCase):
    async def test_raw_key_is_uploaded_and_never_added_to_exec_environment(self) -> None:
        environment = AsyncMock()
        environment.exec.return_value.return_code = 0
        environment.exec.return_value.stdout = ""
        uploaded_secret = None

        async def capture_upload(source: Path, destination: str) -> None:
            nonlocal uploaded_secret
            if destination == SylvanderAgent.API_KEY_FILE:
                uploaded_secret = source.read_text()

        environment.upload_file.side_effect = capture_upload
        agent = SylvanderAgent(
            logs_dir=Path("/tmp/logs"),
            model_name="provider/model",
            extra_env={"SYLVANDER_HARBOR_API_KEY": "secret-value"},
        )

        await agent.run("task", environment, AsyncMock())

        uploaded = environment.upload_file.call_args.args
        self.assertEqual(uploaded[1], agent.API_KEY_FILE)
        self.assertEqual(uploaded_secret, "secret-value")
        for call in environment.exec.await_args_list:
            self.assertNotIn("secret-value", str(call))


if __name__ == "__main__":
    unittest.main()
