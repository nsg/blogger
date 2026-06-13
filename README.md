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
- **Voice dictation** — record from the editor and insert transcribed text at the cursor
- **Live Zola preview** rendered in a side pane via Podman
- **Web search and fetch** — the AI can look things up while helping you write
- **Auto-save** with atomic writes so Zola never sees a truncated file
- **Auto-create posts** — pass a non-existing path and Blogger creates the file with front matter and any missing `_index.md` sections
- **Resizable three-pane layout** — reference browser, editor, and assistant

## Quick Start

```bash
# Install build dependency (Debian/Ubuntu)
sudo apt install libdbus-1-dev

# Clone and install
git clone <repo-url> && cd blogger
./install.sh

# Store your Ollama API key in the system keyring
blogger set-key

# Store your OpenAI STT API key for voice dictation
blogger set-stt-key

# Open an existing post
blogger ~/blog/site/content/posts/my-post.md

# Or create a new one (auto-generates front matter)
blogger ~/blog/site/content/posts/new-post.md
```

Open `http://localhost:3000` in your browser.

To use Blogger from another device on your local network, start it normally and
copy the short PIN printed in the terminal:

```bash
blogger ~/blog/site/content/posts/my-post.md
```

Then visit the machine's LAN address:

```text
http://<your-lan-ip>:3000
```

Localhost access does not require a PIN. Remote browsers must enter the PIN
within 120 seconds; after that, Blogger stores a persistent HTTP-only session
cookie. The server stores the matching session token in the system keyring, so
authorized browsers remain authorized across Blogger restarts. The Zola preview
is exposed on `http://<your-lan-ip>:1111`.

## Requirements

- **Rust** toolchain (for building)
- **Podman** — runs Zola in a container for live preview
- **D-Bus secret service** (GNOME Keyring, KDE Wallet, etc.) — stores the API key
- **Ollama API key** — powers the AI assistant

## Configuration

### API Keys

Store your Ollama API key securely in the system keyring:

```bash
blogger set-key
```

The key is looked up in this order:

1. `OLLAMA_API_KEY` environment variable (or `.env` file)
2. System keyring (GNOME Keyring, KDE Wallet, macOS Keychain, etc.)

Voice dictation uses OpenAI speech-to-text. Store that key separately:

```bash
blogger set-stt-key
```

The STT key is looked up in this order:

1. `OPENAI_API_KEY` environment variable (or `.env` file)
2. System keyring (GNOME Keyring, KDE Wallet, macOS Keychain, etc.)

### Local Network Access

Blogger binds the web UI to `0.0.0.0:3000`, and the Zola preview container is
published on `0.0.0.0:1111`. Use your machine's LAN address from another
device:

```bash
hostname -I
```

Localhost requests are allowed without authentication. Requests from other
devices require either:

- a valid `blogger_session` cookie, set automatically after entering the startup
  PIN in the browser and retained across Blogger restarts
- an `Authorization: Bearer <session-token>` header, for scripted access using
  the issued session token

The startup PIN is printed to the terminal and is valid for 120 seconds. It is
only needed for new remote browsers or after the persistent session expires.
`/api/health` remains unauthenticated for health checks.

When Blogger is behind an HTTPS reverse proxy, configure the proxy to send
`X-Forwarded-Proto: https`. Blogger uses that header to mark the session cookie
as `Secure`.

## Usage

```
blogger [PATH]
blogger set-key
blogger set-stt-key
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
