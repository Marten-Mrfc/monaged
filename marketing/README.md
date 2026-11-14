# Press kit

Everything for writing or talking about Monaged.

| What | Where |
|---|---|
| Brand guide: the mark, colours, type, voice | [brand.md](brand.md) |
| Launch copy: taglines, descriptions, Show HN, Product Hunt, social posts | [launch.md](launch.md) |
| Logos and the mark (SVG) | [`assets/brand/`](../assets/brand) |
| App icon (ICO and PNG) | [`assets/monaged.ico`](../assets/monaged.ico), [`assets/monaged-256.png`](../assets/monaged-256.png) |
| Link card (1280×640) | [`assets/brand/social-preview.png`](../assets/brand/social-preview.png) |
| Screenshots (3200×2000) | [`site/img/`](../site/img) |
| Demo video | [`site/media/demo.mp4`](../site/media) |
| Website | [`site/`](../site) |

## The short version

**Monaged** is a native Windows app for running Claude Code agents on your projects in parallel. Every agent works in its own git worktree inside WSL; a pipeline takes a feature from a sentence to a merged branch and stops for your approval when a plan is risky; every change comes with a diff, a summary and a confidence score with its reasons. It's free, uses your own Claude account, and is an independent project, not affiliated with Anthropic.

## Facts

- **Platform:** Windows 10 (2004+) and 11, x64, with WSL 2
- **Built with:** Rust and GPUI (Zed's UI framework); Claude Code through claude-agent-acp and the Agent Client Protocol
- **Price:** free; agents use your own Claude subscription or API account
- **License:** MIT with the Commons Clause (source-available)
- **Current version:** 0.3.0
