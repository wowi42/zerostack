use compact_str::CompactString;
use rig::agent::{Agent, AgentBuilder};
use rig::completion::CompletionModel;
use rig::providers::openrouter;
use smallvec::SmallVec;

use crate::agent::prompt::{SYSTEM_PROMPT, TODO_TOOLS_PROMPT};
use crate::agent::tools;
use crate::cli::Cli;
use crate::config::Config;
use crate::context::ContextFiles;
#[cfg(feature = "mcp")]
use crate::extras::mcp::McpClientManager;
use crate::permission::ask::AskSender;
use crate::permission::checker::PermCheck;
use crate::sandbox::Sandbox;

#[allow(dead_code)]
pub type ZAgent = Agent<openrouter::CompletionModel>;

#[allow(clippy::too_many_arguments)]
pub async fn build_agent_inner<M: CompletionModel + 'static>(
    model: M,
    cli: &Cli,
    cfg: &Config,
    context: &ContextFiles,
    permission: Option<PermCheck>,
    ask_tx: Option<AskSender>,
    sandbox: Sandbox,
    reasoning_enabled: bool,
    #[cfg(feature = "mcp")] mcp_manager: Option<&McpClientManager>,
) -> Agent<M> {
    let reasoning_prefix = if reasoning_enabled {
        "You reason carefully and think step-by-step.\n\n"
    } else {
        "You respond concisely without showing your reasoning.\n\n"
    };
    let context_agents = context.agents.as_deref().unwrap_or("");
    let context_prompt = context.current_prompt.as_deref().unwrap_or("");
    let cwd = std::env::current_dir()
        .ok()
        .map(|p| p.display().to_string())
        .unwrap_or_default();

    let total_len = reasoning_prefix.len()
        + SYSTEM_PROMPT.len()
        + 1
        + TODO_TOOLS_PROMPT.len()
        + if context.agents.is_some() {
            2 + context_agents.len()
        } else {
            0
        }
        + if context.current_prompt.is_some() {
            6 + context_prompt.len()
        } else {
            0
        }
        + if !cwd.is_empty() { 30 + cwd.len() } else { 0 };

    #[cfg(feature = "memory")]
    let total_len = total_len
        + context.memory.as_deref().map_or(0, |m| m.len() + 8) // "\n\n---\n\n" + content
        + crate::agent::prompt::MEMORY_TOOLS_PROMPT.len();

    let mut preamble = String::with_capacity(total_len);
    preamble.push_str(reasoning_prefix);
    preamble.push_str(SYSTEM_PROMPT);
    preamble.push('\n');
    preamble.push_str(TODO_TOOLS_PROMPT);
    if !context_agents.is_empty() {
        preamble.push_str("\n\n");
        preamble.push_str(context_agents);
    }
    if !context_prompt.is_empty() {
        preamble.push_str("\n\n---\n\n");
        preamble.push_str(context_prompt);
    }
    if !cwd.is_empty() {
        preamble.push_str("\n\nCurrent working directory: ");
        preamble.push_str(&cwd);
    }
    #[cfg(feature = "memory")]
    {
        crate::agent::memory::append_memory_block(&mut preamble, context.memory.as_deref());
        preamble.push_str(crate::agent::prompt::MEMORY_TOOLS_PROMPT);
    }

    let mut builder = AgentBuilder::new(model).preamble(&preamble);

    let max_tokens = cli.resolve_max_tokens(cfg);
    builder = builder.max_tokens(max_tokens);

    let max_turns = cli.resolve_max_agent_turns(cfg);
    builder = builder.default_max_turns(max_turns);

    if let Some(temp) = cli.temperature {
        let clamped = temp.clamp(0.0, 2.0);
        builder = builder.temperature(clamped);
    }

    if cli.resolve_no_tools(cfg) {
        builder.build()
    } else {
        let max_text_file_size = cfg.max_text_file_size;
        let base_tools: SmallVec<[Box<dyn rig::tool::ToolDyn>; 8]> = SmallVec::from_buf([
            Box::new(tools::ReadTool::new(
                permission.clone(),
                ask_tx.clone(),
                max_text_file_size,
            )),
            Box::new(tools::WriteTool::new(
                permission.clone(),
                ask_tx.clone(),
                max_text_file_size,
            )),
            Box::new(tools::EditTool::new(permission.clone(), ask_tx.clone())),
            Box::new(tools::BashTool::new(
                permission.clone(),
                ask_tx.clone(),
                sandbox.clone(),
            )),
            Box::new(tools::GrepTool::new(permission.clone(), ask_tx.clone())),
            Box::new(tools::FindFilesTool::new(
                permission.clone(),
                ask_tx.clone(),
            )),
            Box::new(tools::ListDirTool::new(permission.clone(), ask_tx.clone())),
            Box::new(tools::WriteTodoList::new(
                permission.clone(),
                ask_tx.clone(),
            )),
        ]);

        let mut builder = builder.tools(base_tools.into_vec());
        #[cfg(feature = "memory")]
        {
            use crate::agent::memory::{MemoryRead, MemorySearch, MemoryWrite};
            builder = builder
                .tool(MemoryWrite::new(permission.clone(), ask_tx.clone()))
                .tool(MemoryRead::new(permission.clone(), ask_tx.clone()))
                .tool(MemorySearch::new(permission.clone(), ask_tx.clone()));
        }

        #[cfg(feature = "mcp")]
        if let Some(manager) = &mcp_manager {
            let allow_all = cfg.allow_all_mcp_calls.unwrap_or(false);
            if allow_all {
                if let Some(ref perm) = permission {
                    perm.lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .set_allow_all_mcp_calls(true);
                }
            }
            let mcp_tools = manager
                .collect_tools(permission.clone(), ask_tx.clone())
                .await;
            if !mcp_tools.is_empty() {
                let dyn_tools: Vec<Box<dyn rig::tool::ToolDyn>> = mcp_tools
                    .into_iter()
                    .map(|t| Box::new(t) as Box<dyn rig::tool::ToolDyn>)
                    .collect();
                builder = builder.tools(dyn_tools);
            }
        }

        builder.build()
    }
}

#[allow(dead_code)]
pub fn create_client(api_key: Option<&str>) -> anyhow::Result<openrouter::Client> {
    let key = api_key
        .map(CompactString::new)
        .or_else(|| {
            std::env::var("OPENROUTER_API_KEY")
                .ok()
                .map(CompactString::new)
        })
        .ok_or_else(|| {
            anyhow::anyhow!(
                "No API key found. Set OPENROUTER_API_KEY environment variable or pass --api-key."
            )
        })?;
    Ok(openrouter::Client::new(String::from(key))?)
}
