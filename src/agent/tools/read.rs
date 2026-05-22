use rig::completion::ToolDefinition;
use rig::tool::Tool;

use crate::agent::tools::{AskSender, PermCheck, ReadArgs, ToolError, check_perm_path};

const DEFAULT_MAX_TEXT_SIZE: u64 = 1024 * 1024;

pub struct ReadTool {
    pub permission: Option<PermCheck>,
    pub ask_tx: Option<AskSender>,
    pub max_text_file_size: u64,
}

impl ReadTool {
    pub fn new(
        permission: Option<PermCheck>,
        ask_tx: Option<AskSender>,
        max_text_file_size: Option<u64>,
    ) -> Self {
        ReadTool {
            permission,
            ask_tx,
            max_text_file_size: max_text_file_size.unwrap_or(DEFAULT_MAX_TEXT_SIZE),
        }
    }
}

impl Tool for ReadTool {
    const NAME: &'static str = "read";

    type Error = ToolError;
    type Args = ReadArgs;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: "read".to_string(),
            description: "Read the contents of a file. Supports text files. Defaults to first 2000 lines. Use offset/limit for large files.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Path to the file (relative or absolute)" },
                    "offset": { "type": "integer", "description": "Line number to start from (1-indexed)" },
                    "limit": { "type": "integer", "description": "Maximum number of lines to read" }
                },
                "required": ["path"]
            }),
        }
    }

    async fn call(&self, args: ReadArgs) -> Result<String, ToolError> {
        check_perm_path(&self.permission, &self.ask_tx, "read", &args.path).await?;

        let metadata = tokio::fs::metadata(&args.path).await?;
        let file_size = metadata.len();
        if file_size > self.max_text_file_size {
            return Err(ToolError::Msg(format!(
                "File too large ({} bytes). Maximum allowed file size is {} bytes.",
                file_size, self.max_text_file_size
            )));
        }
        let content = tokio::fs::read_to_string(&args.path).await?;
        let total_lines = content.lines().count();

        let offset = args.offset.unwrap_or(1).max(1) - 1;
        let limit = args.limit.unwrap_or(2000);
        let end = (offset + limit).min(total_lines);

        let excerpt: String = content
            .lines()
            .skip(offset)
            .take(end - offset)
            .collect::<Vec<_>>()
            .join("\n");
        let info = format!(
            "File: {} ({} lines total, showing lines {}-{})\n\n{}",
            args.path,
            total_lines,
            offset + 1,
            end,
            excerpt
        );
        Ok(info)
    }
}
