# Slash Commands

All slash commands are available from the TUI input prompt.

## Session

| Command | Description |
| ------- | ----------- |
| `/clear` | Clear the current session (all messages, tokens, compactions). |
| `/undo` | Remove the last exchange (user message + assistant response). |
| `/retry` | Load the last user message into the input editor for editing. |
| `/quit` | Exit zerostack. |
| `/sessions` | List recent saved sessions (up to 20). |
| `/sessions <id-prefix>` | Load a session by its ID prefix. |
| `/sessions delete <id-prefix>` | Delete a session by its ID prefix. |
| `/history` | Show global chat history (last 10 entries across sessions). |

## Provider & Model

| Command | Description |
| ------- | ----------- |
| `/provider` | Show the current provider. |
| `/provider <name>` | Switch to a different provider. |
| `/model` | Show the current model. |
| `/model <name>` | Switch to a different model. |
| `/models` | List all quick models defined in config. |
| `/models <name>` | Switch to a named quick model. |
| `/models-add <name> <provider> <model>` | Save a new quick model to the config file. |

## Security

| Command | Description |
| ------- | ----------- |
| `/mode` | Show the current security mode. |
| `/mode standard` | Allow path tools within CWD, ask for external paths. Config rules apply. |
| `/mode restrictive` | Ask for every operation. Config rules skipped. |
| `/mode readonly` | Allow reads only; deny writes, edits, bash, and everything else. |
| `/mode guarded` | Allow reads; ask for writes, edits, bash, and everything else. Config rules apply. |
| `/mode yolo` | Allow everything; ask for destructive bash commands. Config rules apply. |

Prompts can set the security mode automatically via `%%mode=<mode>` on
the first line. When a prompt with `%%mode=last_user_mode` is activated,
the mode reverts to whatever was last set explicitly by `/mode` or
startup config. See Prompts & Themes below.

## Prompts & Themes

| Command | Description |
| ------- | ----------- |
| `/prompt` | List available prompts. |
| `/prompt <name>` | Activate a named prompt. Also applies `%%mode=` from the prompt file if present (see below). |
| `/prompt default` | Clear the active prompt. |

Prompts may include a `%%mode=<mode>` directive on the **first line** to
automatically switch the security mode when activated. Valid modes:
`standard`, `restrictive`, `readonly`, `guarded`, `yolo`. Use
`%%mode=last_user_mode` to restore the mode the user last set via `/mode`
or startup config. The directive line is stripped from the prompt content
before it reaches the agent.

Example `ask.md`:
```markdown
%%mode=readonly

## Read-Only Mode

You are in read-only mode. Only read files and explore.
```
| `/theme` | List available themes. |
| `/theme <name>` | Activate a named theme. |
| `/theme default` | Clear the active theme (use config colors). |
| `/regen-prompts` | Restore built-in prompts to the prompts directory. |
| `/regen-themes` | Restore built-in themes to the themes directory. |

## Conversation

| Command | Description |
| ------- | ----------- |
| `/compress [instructions]` | Compress conversation history to free context window space. |
| `/compact` | Alias for `/compress`. |
| `/editsys` | Show the current edit system mode (similarity or hashedit). |
| `/editsys similarity` | Use SEARCH/REPLACE with fuzzy matching for edits (default). |
| `/editsys hashedit` | Use CRC-32 tag-based edits (token-efficient, CAS-guarded). |
| `/btw <message>` | Ask the agent a question without adding it to the chat history. Neither the question nor the response is saved. |
| `/reasoning` | Toggle LLM reasoning on/off (requires model support). |
| `/thinking` | Alias for `/reasoning`. |
| `/toggle` | Show available toggleable features. |
| `/toggle todo [on\|off]` | Enable or disable todo-list tools. |

## MCP (feature-gated)

| Command | Description |
| ------- | ----------- |
| `/mcp` | List connected MCP servers and their tool counts. |
| `/mcp <server>` | List tools of a specific MCP server. |

## Worktree (feature-gated)

| Command | Description |
| ------- | ----------- |
| `/worktree <name>` | Create a git worktree on a new branch and `cd` into it. |
| `/wt-merge [branch]` | Merge the worktree branch back into the target branch. |
| `/wt-exit` | Exit the worktree and return to the main repo. |

## Loop (feature-gated)

| Command | Description |
| ------- | ----------- |
| `/loop [prompt]` | Start the iterative coding loop. |
| `/loop stop` | Stop the active loop. |
| `/loop status` | Show current loop status. |

## General

| Command | Description |
| ------- | ----------- |
| `/help` | Show the full help message listing all commands and keybindings. |

## Keybindings

| Shortcut | Action |
| -------- | ------ |
| `Enter` | Send message. |
| `Shift+Enter` | Insert newline. |
| `Ctrl+C` | Cancel current agent response or quit. |
| `Ctrl+D` | Send message (alternative). |
| `Ctrl+W` | Delete word backwards. |
| `Ctrl+U` | Delete to beginning of line. |
| `Ctrl+L` | Clear terminal. |
| `Ctrl+G` | Open the current input in the system editor (`$EDITOR`). |
| `Ctrl+S` | Save session. |
| `Tab` | Activate file picker / auto-complete paths. |
| `Up / Down` | Navigate command history. |
| `PageUp / PageDown` | Scroll viewport. |
| `Home / End` | Jump to start/end of input. |
| `Alt+Enter` | Retry last prompt. |
| `Escape` | Close active picker / cancel. |
