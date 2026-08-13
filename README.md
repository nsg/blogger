<div align="center">
  <h1>Blogger</h1>
  <p>A self-hosted writing environment for Zola blogs.</p>
</div>

---

## About

Blogger is a self-hosted writing environment for a Zola blog. It combines a Monaco editor, an AI writing assistant, dictation, and a live preview in one browser interface.

Run Blogger as a container in front of an existing Git checkout of the blog. Blogger edits that writable checkout and starts Zola for previewing; the deployment remains responsible for providing and backing up the checkout.

## Features

- Browse and manage posts from an archive panel.
- Edit multiple posts with automatic saves and external-change conflict detection.
- Create and preview draft posts.
- Share a versioned writing-style profile and search, create, or revise drafts
  from Claude.ai chat or Claude Mobile voice mode through the protected remote
  MCP connector.
- Review writing with an AI assistant and insert speech-to-text dictation.
- Preview the site through an authenticated live Zola view.
- Commit all checkout changes and push them to GitHub manually.
- Protect the interface with a single-password login.

## Quick Start

Run these commands from the blog checkout. Ensure the checkout is writable by UID 1000, which the container uses for its `blogger` user.

```bash
export OLLAMA_API_KEY='replace-with-ollama-key'
export OLLAMA_MODEL='qwen3.5:397b'
export OPENAI_API_KEY='replace-with-openai-key'
export BLOGGER_PASSWORD='replace-with-login-password'
export BLOGGER_SESSION_SECRET="$(openssl rand -hex 32)"
export GITHUB_TOKEN='replace-with-github-token'
export BLOGGER_GIT_NAME='Your Name'
export BLOGGER_GIT_EMAIL='you@example.com'
export BLOGGER_MCP_PUBLIC_URL='https://mcp.example.com/mcp'

docker run --rm \
  --publish 127.0.0.1:3000:3000 \
  --publish 127.0.0.1:3001:3001 \
  --mount type=bind,src="$PWD",dst=/data \
  --workdir /data \
  --env OLLAMA_API_KEY \
  --env OLLAMA_MODEL \
  --env OPENAI_API_KEY \
  --env BLOGGER_PASSWORD \
  --env BLOGGER_SESSION_SECRET \
  --env GITHUB_TOKEN \
  --env BLOGGER_GIT_NAME \
  --env BLOGGER_GIT_EMAIL \
  --env BLOGGER_MCP_PUBLIC_URL \
  ghcr.io/nsg/blogger
```

Open `http://localhost:3000`. Put Blogger behind an HTTPS reverse proxy when exposing it outside the host.

### Install on Android or desktop

Open Blogger in Chrome, sign in, and choose **Install app** (or **Add to Home screen**) from the browser menu. Blogger then launches in its own app window from the home screen or application launcher. Launching the icon again focuses the existing Blogger window on Chromium browsers that support launch handling.

Installation requires HTTPS when Blogger is accessed from another device; `localhost` is the browser-supported exception. Blogger remains network-dependent and does not cache posts or application data for offline use.

### Running from source

Install Node.js 22, the stable Rust toolchain, and Zola, and ensure `zola` is on `PATH`. Build the embedded frontend from the Blogger source checkout:

```bash
cd /path/to/blogger/frontend
npm ci
npm run build
```

Export the variables shown above, then run Blogger from the blog checkout. Using `--manifest-path` keeps the blog checkout as the process working directory, so no search-root argument is needed.

```bash
cd /path/to/blog/checkout
cargo run --locked --manifest-path /path/to/blogger/Cargo.toml
```

## Configuration

All environment variables except `OLLAMA_MODEL` are required and must be non-empty.
Set `OLLAMA_MODEL` at deployment time to change the writing assistant model; it
defaults to `qwen3.5:397b` when omitted.

| Variable | Description |
| --- | --- |
| `OLLAMA_API_KEY` | Authenticates requests from the AI writing assistant to Ollama. |
| `OLLAMA_MODEL` | Selects the Ollama model used by the writing assistant. Defaults to `qwen3.5:397b`. |
| `OPENAI_API_KEY` | Authenticates speech-to-text requests for dictation. |
| `BLOGGER_PASSWORD` | Sets the plaintext password for the single-user login. |
| `BLOGGER_SESSION_SECRET` | Signs login sessions; provide exactly 64 hexadecimal characters representing 32 random bytes. |
| `GITHUB_TOKEN` | Authenticates HTTPS Git operations against the configured GitHub origin. |
| `BLOGGER_GIT_NAME` | Sets the author and committer name for commits Blogger creates. |
| `BLOGGER_GIT_EMAIL` | Sets the author and committer email for commits Blogger creates. |
| `BLOGGER_MCP_PUBLIC_URL` | Canonical public HTTPS URL of the MCP endpoint, ending in `/mcp`, for example `https://mcp.example.com/mcp`. |

Generate a session secret outside Blogger and keep it stable across restarts:

```bash
openssl rand -hex 32
```

Rotating `BLOGGER_SESSION_SECRET` invalidates existing login sessions.

### GitHub token

Create a fine-grained GitHub personal access token. Restrict its repository access to the blog repository and grant only the repository `Contents` permission with read and write access. Supply the token as `GITHUB_TOKEN`; keep the checkout's `origin` URL as ordinary HTTPS without embedded credentials.

### Repository requirements

- Provide a writable, non-bare Git checkout including its `.git` directory. The discovered Zola site must be inside that checkout.
- Check out a normal branch and configure it to track the matching `origin/<branch>` upstream.
- Configure `origin` with an HTTPS URL.
- Provide a Zola site with `content/post`. Set `[slugify].paths = "on"` in `config.toml`, or omit that setting to use Zola's supported default.
- Allow Blogger to write the Zola site root, `content/post`, and `static/images`.

The deployment must populate the checkout before Blogger starts; Blogger never clones it. Run exactly one Blogger replica against a checkout. Multiple processes or replicas sharing one working tree are unsupported.

## Usage

```text
blogger [SEARCH_ROOT]
```

`SEARCH_ROOT` must be a directory. Blogger recursively scans it and uses the first `config.toml` it finds as the Zola site root. If omitted, `SEARCH_ROOT` defaults to the current working directory. Startup fails if the site, writable content paths, required environment, Git repository contract, or Zola process is unavailable.

### Ports and probes

| Port | Access | Service |
| --- | --- | --- |
| 3000 | Private | Blogger web UI and authenticated preview proxy. |
| 3001 | Public through HTTPS proxy | MCP connector and its OAuth endpoints. |
| 1111 | Internal on `127.0.0.1` | Zola live preview. Do not expose this port. |

Use `GET /api/health` as the liveness probe. It reports that the Blogger process is serving requests. Use `GET /api/ready` as the readiness probe; it succeeds after the private Zola preview responds. Both endpoints are unauthenticated.

## Claude remote connector

Blogger provides a remote MCP connector for Claude.ai and Claude Mobile. It
exposes `list_archive`, `list_tags`, `search_posts`, and `get_post` for reading
published posts and drafts. `list_archive` returns only post titles and
`list_tags` returns unique tags as compact overviews; `search_posts` matches
titles as well as the complete Markdown content.
`get_writing_style` reads the complete `WRITING_STYLE.md` file at the Zola site
root, and `replace_writing_style` creates or replaces that file as a single
Markdown document. A useful profile can cover voice and tone, structure and
pacing, vocabulary, formatting conventions, examples, and patterns to avoid.
Because the file is inside the blog checkout, Blogger's normal **Commit and
push** flow versions it alongside the blog.

The connector also exposes four draft-only writing tools:

- `create_draft` creates a new draft from raw TOML front matter and a separate
  Markdown body. The optional slug is generated from the title when omitted.
- `append_draft` appends dictated text, with an optional newline or blank-line
  separator.
- `edit_draft` atomically applies exact, unique text replacements to the body.
- `replace_draft` replaces the complete front matter and body.

Send front matter as raw TOML without `+++` delimiters. Blogger validates it and
always writes `draft = true`. Draft updates require the exact revision returned
by `get_post` or the previous write. Writing-style replacements likewise
require the revision returned by `get_writing_style`; omit it only when that
tool returns a null revision. These checks prevent silent overwrites. The
connector cannot modify published posts, publish, delete, rename, manage images,
commit, push, or access arbitrary files other than the fixed writing-style file.

Add the value of `BLOGGER_MCP_PUBLIC_URL` as a custom connector in Claude.ai.
Claude discovers Blogger's OAuth endpoints automatically. The authorization page
is served by Blogger and accepts `BLOGGER_PASSWORD`; the password is never sent
to Claude. Authorization grants `posts:read` and `posts:write`. Write access is
limited to drafts and the fixed writing-style guide; it cannot access the
private web interface or REST API.

After upgrading an existing read-only deployment, reauthorize the connector so
Claude receives `posts:write`. Existing read-only tokens never acquire write
access automatically.

### Deployment contract

Kubernetes resources, DNS, and certificates are intentionally maintained outside
this repository. The external deployment must:

- route the public MCP hostname to container port `3001`, while keeping port
  `3000` private;
- terminate TLS so `BLOGGER_MCP_PUBLIC_URL` is reachable over public HTTPS;
- preserve the original HTTP `Host` header, which Blogger validates against the
  configured public URL;
- route the entire dedicated hostname, including `/mcp`, `/authorize`, `/token`,
  `/register`, `/revoke`, and `/.well-known/*`, to port `3001`;
- support Streamable HTTP responses (`text/event-stream`), disable response
  buffering for `/mcp`, and allow long-lived HTTP requests;
- rate-limit public `POST /authorize` requests per source at the ingress in
  addition to Blogger's built-in password-attempt limiter; and
- continue using the private port `3000` endpoints for liveness and readiness
  probes.

The port `3001` listener does not mount the Blogger frontend, preview proxy, or
private REST API, so routing the entire dedicated hostname does not expose them.
Do not route a path-stripped `/mcp` prefix: Blogger expects the public endpoint to
arrive as `/mcp`.

Blogger uses in-memory MCP sessions and OAuth grants, consistent with its
single-replica deployment requirement. A pod restart disconnects active MCP
sessions and invalidates access and refresh tokens; reconnect and authorize the
connector again. The dynamically registered Claude client ID is stable across
restarts as long as `BLOGGER_SESSION_SECRET` remains unchanged. Rotating that
secret requires removing and re-adding the connector in Claude.ai.

### Git publication

Automatic saves only update the checkout. **Commit and push** fetches the upstream, checks for overlapping local and remote paths, shows all checkout changes, creates one commit with the confirmed subject, rebases remote-only changes when safe, and pushes the checked-out branch. If the push fails after the commit, use **Retry push**; Blogger does not create a duplicate commit.

**Sync from GitHub** fetches and fast-forwards the branch only when the working tree is clean, no local commits are unpushed, and the histories have not diverged. Blogger never syncs automatically.

### Drafts

Blogger starts Zola with `--drafts`, so draft posts appear in the archive and preview. Publish a draft by editing its TOML front matter in Monaco and removing `draft = true` or changing it to `draft = false`.

## License

MIT
