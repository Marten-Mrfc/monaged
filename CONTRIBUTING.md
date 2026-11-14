# Contributing to Monaged

Thanks for wanting to help. Bug reports, ideas, docs and code are all welcome.

## Reporting bugs and asking for features

Use the issue templates. For a bug, the most useful things are:

- your Monaged version (Settings → Updates) and Windows version;
- what the **health check** says (Ctrl K → Health check), with "Copy details" where there is one;
- the end of `%APPDATA%\Monaged\monaged.log`;
- the smallest steps that make it happen. The playground project (Ctrl K → Create the playground project) is a good place to reproduce things.

Security problems go through [SECURITY.md](SECURITY.md), not public issues.

## Building from source

Monaged is a Windows app written in Rust with [GPUI](https://crates.io/crates/gpui).

1. Install [Rust](https://rustup.rs) (stable) and Visual Studio's **Desktop development with C++** workload (it brings the Windows SDK, whose `fxc.exe` compiles GPUI's shaders and whose `rc.exe` embeds the icon).
2. Clone the repository and run it:

   ```powershell
   cargo run
   ```

   Debug builds keep a console window for logs. Options: `--distro <name>`, `--agent <path to an ACP agent in WSL>`, and `--setup` to open the setup guide.
3. Run the tests:

   ```powershell
   cargo test
   ```

4. A release build and the installer files:

   ```powershell
   ./scripts/release.ps1
   ```

   This writes `dist\Monaged-Setup-<version>.exe`, a portable zip and `SHA256SUMS.txt`.

Monaged only runs on Windows. From Linux, `cargo check --target x86_64-pc-windows-gnu` (with mingw-w64 installed) type-checks it, which is handy for quick checks.

## Finding your way around

[docs/architecture.md](docs/architecture.md) explains how the pieces fit together and lists every source file with what it does. [ROADMAP.md](ROADMAP.md) has the decisions behind the design, and what's planned.

## Code style

- Read the code around your change and match it: names, the comment style (comments say why, in plain words), and how views are built from the component kit in `src/ui.rs` and the tokens in `src/theme.rs`. Views ask for a role (`theme::text_2()`), never a hex value.
- One accent: red only ever means "needs you". Hairlines instead of boxes, two font weights.
- Messages in the UI are short, plain sentences: say what happened and what to do about it.
- Anything that runs in WSL goes through `src/wsl.rs` (scripts go over stdin; `wsl.exe` drops double quotes from arguments).
- Add unit tests for logic that doesn't need a window (parsing, rules, scoring); see `src/tests.rs` and the `tests` modules.
- `cargo test` and `cargo clippy` should pass. CI runs both on Windows for every push.

## Pull requests

- Keep each one to one change, with a description of what it does and why, and how you tested it (screenshots for anything visible).
- Update `CHANGELOG.md` under an "Unreleased" heading.
- By contributing, you agree your contribution is licensed under this project's license (MIT with the Commons Clause, see [LICENSE](LICENSE)).

## Releasing (maintainers)

See [docs/releasing.md](docs/releasing.md).
