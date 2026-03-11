<div align="center">
  <h1>Blogger</h1>
  <p>AI-powered writing environment for Zola blogs.</p>
</div>

---

## About

Blogger is a local writing tool that combines a Monaco editor, an AI assistant, and a live Zola preview in a three-pane layout. Point it at a markdown file in your Zola blog and start writing — the AI reviews your paragraphs, suggests edits, and can search the web to verify facts.

## Features

- **Monaco editor** with Zola front matter syntax highlighting and word count
- **AI writing assistant** with paragraph-level feedback and "Apply fix" buttons
- **Live Zola preview** rendered in a side pane via Podman
- **Web search and fetch** — the AI can look things up while helping you write
- **Auto-save** with atomic writes so Zola never sees a truncated file
- **Auto-create posts** — pass a non-existing path and Blogger creates the file with front matter and any missing `_index.md` sections
- **Resizable three-pane layout** — reference browser, editor, and assistant

## Quick Start

```bash
# Clone and install
git clone <repo-url> && cd blogger
./install.sh

# Set your Ollama API key
export OLLAMA_API_KEY="your-key-here"

# Open an existing post
blogger ~/blog/site/content/posts/my-post.md

# Or create a new one (auto-generates front matter)
blogger ~/blog/site/content/posts/new-post.md
```

Open `http://localhost:3000` in your browser.

## Requirements

- **Rust** toolchain (for building)
- **Podman** — runs Zola in a container for live preview
- **Ollama API key** — powers the AI assistant

## Configuration

| Variable | Description | Default |
|---|---|---|
| `OLLAMA_API_KEY` | API key for Ollama chat, search, and fetch | *(required)* |

Place it in a `.env` file in the working directory or export it in your shell.

## Usage

```
blogger [PATH]
```

**PATH** can be:

| Input | Behavior |
|---|---|
| Existing `.md` file | Opens it in the editor, starts Zola preview |
| Non-existing `.md` file | Creates it with front matter, then opens it |
| Directory | Treats it as the blog root, starts Zola preview |
| *(omitted)* | Starts the editor with default content, no preview |

The tool detects your Zola site by walking up from the file looking for a `site/` directory or `config.toml`.

### Ports

| Port | Service |
|---|---|
| 3000 | Blogger web UI |
| 1111 | Zola preview (Podman container) |

## License

MIT
