use std::env;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use sylvander_llm_openai::{
    OpenAiProtocol, OpenAiProvider, OpenAiProviderConfig, ProviderFeatures,
};
use sylvander_testbench_agent::harbor::{HarborRunConfig, run_harbor_task};
use url::Url;

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("sylvander Harbor adapter failed: {error}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), String> {
    let arguments = Arguments::parse(env::args().skip(1))?;
    let api_key = env::var("SYLVANDER_HARBOR_API_KEY")
        .map_err(|_| "SYLVANDER_HARBOR_API_KEY is required".to_string())?;
    let environment_isolated = env::var("SYLVANDER_HARBOR_ISOLATED").as_deref() == Ok("true");
    let provider_id =
        env::var("SYLVANDER_HARBOR_PROVIDER_ID").unwrap_or_else(|_| "minimax-cn".into());
    let model_id = env::var("SYLVANDER_HARBOR_MODEL_ID").unwrap_or_else(|_| "MiniMax-M2.7".into());
    let base_url = env::var("SYLVANDER_HARBOR_BASE_URL")
        .unwrap_or_else(|_| "https://api.minimaxi.com/v1".into());
    let provider = OpenAiProvider::new_with_timeout(
        OpenAiProviderConfig {
            provider_id: provider_id.clone(),
            base_url: Url::parse(&base_url).map_err(|_| "invalid provider base URL")?,
            api_key,
            protocol: OpenAiProtocol::ChatCompletions,
            features: ProviderFeatures::default(),
        },
        Duration::from_secs(arguments.timeout_secs),
    )
    .map_err(|error| error.to_string())?;
    let instruction = tokio::fs::read_to_string(&arguments.instruction_file)
        .await
        .map_err(|error| format!("failed to read instruction: {error}"))?;
    let session_id = format!(
        "harbor-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| "system clock precedes Unix epoch")?
            .as_millis()
    );
    let trajectory = run_harbor_task(
        Arc::new(provider),
        HarborRunConfig {
            session_id,
            provider_id,
            model_id,
            workspace: arguments.workspace,
            instruction,
            max_iterations: arguments.max_iterations,
            max_output_tokens: arguments.max_output_tokens,
            timeout: Duration::from_secs(arguments.timeout_secs),
            environment_isolated,
        },
    )
    .await
    .map_err(|error| error.to_string())?;
    let final_answer = trajectory
        .steps
        .iter()
        .rev()
        .find(|step| step.source == sylvander_testbench_agent::Source::Agent)
        .map_or("", |step| step.message.as_str());
    let encoded = serde_json::to_vec_pretty(&trajectory)
        .map_err(|error| format!("failed to encode trajectory: {error}"))?;
    tokio::fs::write(arguments.trajectory_file, encoded)
        .await
        .map_err(|error| format!("failed to write trajectory: {error}"))?;
    tokio::fs::write(arguments.final_answer_file, final_answer)
        .await
        .map_err(|error| format!("failed to write final answer: {error}"))?;
    Ok(())
}

struct Arguments {
    instruction_file: PathBuf,
    trajectory_file: PathBuf,
    final_answer_file: PathBuf,
    workspace: PathBuf,
    max_iterations: u32,
    max_output_tokens: u32,
    timeout_secs: u64,
}

impl Arguments {
    fn parse(arguments: impl Iterator<Item = String>) -> Result<Self, String> {
        let values = arguments.collect::<Vec<_>>();
        let value = |name: &str| {
            values
                .windows(2)
                .find(|pair| pair[0] == name)
                .map(|pair| pair[1].clone())
                .ok_or_else(|| format!("missing required argument {name}"))
        };
        Ok(Self {
            instruction_file: value("--instruction-file")?.into(),
            trajectory_file: value("--trajectory-file")?.into(),
            final_answer_file: value("--final-answer-file")?.into(),
            workspace: value("--workspace")?.into(),
            max_iterations: parse_number(&value("--max-iterations")?, "max iterations")?,
            max_output_tokens: parse_number(&value("--max-output-tokens")?, "max output tokens")?,
            timeout_secs: parse_number(&value("--timeout-secs")?, "timeout seconds")?,
        })
    }
}

fn parse_number<T>(value: &str, name: &str) -> Result<T, String>
where
    T: std::str::FromStr,
{
    value.parse().map_err(|_| format!("invalid {name}"))
}
