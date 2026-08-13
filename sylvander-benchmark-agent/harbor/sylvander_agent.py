"""Harbor BaseAgent adapter for the Sylvander benchmark runner.

Derived from Harbor BaseAgent at commit
ea2fee78517f2e591bad69fcf1e6731f9c23ec99.
"""

import base64
import json
import shlex
from pathlib import Path
from typing import override

from harbor.agents.base import BaseAgent
from harbor.environments.base import BaseEnvironment
from harbor.models.agent.context import AgentContext


class SylvanderAgent(BaseAgent):
    """Run Sylvander inside the task environment and emit native ATIF v1.7."""

    SUPPORTS_ATIF = True
    BINARY = "/opt/sylvander/bin/sylvander-harbor-agent"

    @staticmethod
    @override
    def name() -> str:
        return "sylvander"

    @override
    def version(self) -> str:
        return "0.1.0"

    @override
    async def setup(self, environment: BaseEnvironment) -> None:
        host_binary = self._get_env("SYLVANDER_HARBOR_BINARY_HOST_PATH")
        if host_binary:
            prepare = await environment.exec(
                "mkdir -p /opt/sylvander/bin", user="root", timeout_sec=10
            )
            if prepare.return_code != 0:
                raise RuntimeError("failed to prepare Sylvander binary directory")
            await environment.upload_file(Path(host_binary), self.BINARY)
            chmod = await environment.exec(
                f"chmod 755 {shlex.quote(self.BINARY)}", user="root", timeout_sec=10
            )
            if chmod.return_code != 0:
                raise RuntimeError("failed to install Sylvander benchmark binary")
        result = await environment.exec(
            f"test -x {shlex.quote(self.BINARY)}", timeout_sec=10
        )
        if result.return_code != 0:
            raise RuntimeError(
                "Sylvander benchmark binary is absent from the task image"
            )

    @override
    async def run(
        self,
        instruction: str,
        environment: BaseEnvironment,
        context: AgentContext,
    ) -> None:
        if not self.model_name or "/" not in self.model_name:
            raise ValueError("model name must use provider/model format")
        provider_id, model_id = self.model_name.split("/", 1)
        api_key = self._get_env("SYLVANDER_HARBOR_API_KEY")
        if not api_key:
            raise ValueError("SYLVANDER_HARBOR_API_KEY is required")
        encoded_instruction = base64.b64encode(instruction.encode()).decode()
        workspace = self._get_env("SYLVANDER_HARBOR_WORKSPACE") or "/root"
        base_url = (
            self._get_env("SYLVANDER_HARBOR_BASE_URL")
            or "https://api.minimaxi.com/v1"
        )
        protocol = (
            self._get_env("SYLVANDER_HARBOR_PROTOCOL")
            or "openai_chat_completions"
        )
        provider_features = (
            self._get_env("SYLVANDER_HARBOR_PROVIDER_FEATURES") or ""
        )
        env = {
            "SYLVANDER_HARBOR_API_KEY": api_key,
            "SYLVANDER_HARBOR_PROVIDER_ID": provider_id,
            "SYLVANDER_HARBOR_MODEL_ID": model_id,
            "SYLVANDER_HARBOR_BASE_URL": base_url,
            "SYLVANDER_HARBOR_PROTOCOL": protocol,
            "SYLVANDER_HARBOR_PROVIDER_FEATURES": provider_features,
            "SYLVANDER_HARBOR_ISOLATED": "true",
        }
        prepare = await environment.exec(
            "mkdir -p /logs/agent /tmp/sylvander-harbor && "
            f"printf '%s' '{encoded_instruction}' | base64 -d "
            "> /tmp/sylvander-harbor/instruction.md",
            timeout_sec=10,
        )
        if prepare.return_code != 0:
            raise RuntimeError("failed to prepare Sylvander task instruction")
        result = await environment.exec(
            f"{shlex.quote(self.BINARY)} "
            "--instruction-file /tmp/sylvander-harbor/instruction.md "
            "--trajectory-file /logs/agent/trajectory.json "
            "--final-answer-file /logs/agent/final_answer.txt "
            f"--workspace {shlex.quote(workspace)} "
            "--max-iterations 50 --max-output-tokens 2048 --timeout-secs 300",
            cwd=workspace,
            env=env,
        )
        if result.return_code != 0:
            raise RuntimeError("Sylvander benchmark runner exited non-zero")
        metrics_result = await environment.exec(
            "python3 -c \"import json; "
            "m=json.load(open('/logs/agent/trajectory.json'))['final_metrics']; "
            "print(json.dumps(m))\"",
            timeout_sec=10,
        )
        if metrics_result.return_code == 0 and metrics_result.stdout:
            metrics = json.loads(metrics_result.stdout)
            context.n_input_tokens = metrics.get("total_prompt_tokens")
            context.n_output_tokens = metrics.get("total_completion_tokens")
            context.n_cache_tokens = metrics.get("total_cached_tokens")
            context.metadata = {"atif_schema": "ATIF-v1.7"}


def adapter_path() -> Path:
    """Return this source path for custom `--agent-import-path` packaging."""

    return Path(__file__).resolve()
