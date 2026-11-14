# The Monaged playground

Lunchbox is a tiny office-lunch app, made so you can try every part of
Monaged on something real but small. Things are broken on purpose:

- **a bug**: coupons only discount the first line of an order, so
  `test_coupon_applies_to_the_whole_order` fails;
- **a flaky test**: `test_kitchen_answers_in_time` fails about one run in three;
- **a merge conflict**: `feature/price-format` and `develop` both changed
  `format_price`.

The project has three subprojects: **Order history** and **Price format**
(features) and **Security pass** (standing). Its git remote is a local bare
repository (`~/code/.remotes/monaged-playground.git`), so pushing and syncing
cost nothing and touch nothing outside your machine.

Work through the sections in any order. Anything that runs Claude uses your
plan or API credit like any other session; each step says so.

---

## 1. Talk to an agent

Open **Order history** and click **New agent** (or press **Ctrl N**).

- **Send a message**: "Fix the failing test in tests/test_orders.py" (uses Claude).
- **Several lines**: Shift Enter adds a line. Settings → Message box swaps Enter
  and Ctrl Enter.
- **History**: ↑ on the first line of the message box brings back what you
  sent before.
- **Slash commands**: type `/`. Your project's own commands are there too:
  `/smoke` comes from `.claude/commands/smoke.md`. Tab picks the highlighted one.
- **Mode, model, effort**: the chips under the message box. Try **Plan** mode,
  then ask for the order-history feature: the agent plans before it edits.
- **Stop**: Esc stops the turn.
- **Queue**: send a second message while the agent works. It waits in the
  queue (kept even if you close Monaged) and goes when the turn ends.
- **Questions with a form**: ask "Before you start, ask me three questions
  about the order history using a form." The agent's questions show as a form;
  your answers go back to it.

## 2. Images

- Paste a screenshot into the message box (Ctrl V), or drop
  `docs/screenshot.png` on it, or use the paperclip. The chip shows a thumbnail.
- Ask "What is on this page, and what's missing for an order history?"
- The image stays in the conversation as a thumbnail (also after you reopen
  the session). Click it to see it full size; Esc closes it.

## 3. Read the work

- **Syntax colors**: code in replies and in diffs is colored per language.
- **Changes**: the agent's header → Changes (or right-click the agent → Show
  changes). Click a line to comment on it; **Send to …** gives them all to the
  agent at once.
- **All changes**: right-click a subproject → All changes: every agent's
  diff in one view.
- **Summary and review**: after each turn Haiku writes a summary and a
  confidence score (Settings → Summaries and reviews).
- **Timeline**: Inspector (Ctrl Alt B) → Timeline: what happened, when.
- **Replay**: right-click the agent → Replay turn by turn. ← and → step through
  the turns with each turn's diff.
- **Side chat**: the side-chat button in the agent's header (Ctrl J). Ask
  Haiku "What did the agent change, and what could break?" Nothing you ask
  there reaches the agent or its context.

## 4. Undo

Every turn is checkpointed before it starts (in git, without touching your
branch history).

- Point at one of your messages: the ↶ button rolls the worktree back to how it
  was before that message. The next message tells the agent what you undid.
- Right-click the agent → **Undo the last turn's changes…**
- After a rollback, the toast has **Undo** to put everything back.

## 5. Run it

Inspector → **Run** (commands come from `monaged.json`).

- **Start** the dev server: its output streams in, the port is read from it,
  and **Open** shows the app at http://localhost:8765.
- **Run tests**: one test fails (the coupon bug).
- **Run 3×**: the kitchen test passes some runs and fails others, so it's
  marked flaky.
- With an agent's worktree picked at the top of the tab, **Send failures to …**
  hands the failing tests to that agent with their output.

## 6. Project settings

Right-click **Monaged playground** → Project settings.

- **MCP servers**: `lunchbox-menu` is already there. Click **Test**: it lists
  `list_menu` and `price_of`. New sessions get it; ask an agent "Use the
  lunchbox-menu tools: what's vegan, and what does the club cost?"
- **Knowledge**: edit `CLAUDE.md`, the `add-menu-item` skill, the
  `menu-reviewer` subagent and the `/smoke` command, or make new ones. **Shared
  notes** reach every worktree without being committed.
- **Run and test**: override the commands from `monaged.json`.

## 7. Pipelines

Open **Order history** → **Run pipeline**. Describe: "Keep each person's
order history and show it at /history." (uses Claude, several agents)

- Research and verification run first; then a plan.
- **Edit the plan** before you approve it: rename, drop or add workstreams.
- Builders work in parallel worktrees; the Board shows what they share and
  claim. Rerun a stage from the Pipeline tab if you don't like it.

## 8. Money

- Right-click **Security pass** → **Set a spending cap…** (try $1). When the
  subproject reaches it, its agents pause and tell you; raise or remove the cap
  to go on.
- Settings → Usage and budget: your weekly share for all of Monaged.

## 9. Git

Inspector → **Git**.

- **History** with the branch graph; **Commit** from Monaged (Haiku can
  suggest the message); **Fetch / Pull / Push** against the local remote.
- **Merge**: open **Price format**, Inspector → Merge → **Merge into
  develop**. It stops at the prepared conflict without changing anything.
  The **Git** tab then lists `lunchbox/menu.py`: **keep develop** or **take
  feature/price-format**, then **Merge with these choices**. Or **Hand to an
  agent** to combine both versions.
- The merge queue moves work develop → acceptance → main; `develop` merges by
  itself, `acceptance` asks, `main` needs your confirmation.

## 10. Find things

- **Ctrl K**: everything: agents, files, commands, layouts.
- **Ctrl F**: find in this conversation. **Ctrl Shift F**: search every agent.
- **Ctrl 0**: move through the sidebar with the keyboard: ↑ ↓ move, → opens a
  level, ← goes back, Enter opens, Esc leaves.
- **Rename / archive**: right-click an agent. Archived agents fold away under
  their subproject and can be restored.

## 11. Make it yours

- **Layouts**: drag the edges of the sidebar, the inspector and the panes
  (double-click an edge to reset). Ctrl K → Save this layout. **F11** is zen
  mode: one conversation, nothing else.
- **Settings → Keyboard**: change any shortcut (click Change, press the keys).
- **Settings → Appearance**: themes. **General**: density.
- **Settings → Notifications**: Windows notifications when an agent finishes
  or needs you while Monaged is in the background.
- **Health check** (Ctrl K → Health check): WSL, the agent, git, logins, disk.
- **Updates**: Settings → Updates.

## 12. Secrets

Right-click **Monaged playground** → Project settings → **Environment**.

- **Around the repo** already finds a gap: Lunchbox's `.gitignore` doesn't
  cover `.env` files. Click **Add to .gitignore**.
- Add `LUNCHBOX_API_KEY` (any test value, marked **secret**) and `PORT=8765`.
  Values are encrypted by Windows for your account; the database only holds
  the encrypted form.
- The eye shows a value for 15 seconds; copying keeps it out of Win+V history
  and clears it after 30 seconds.
- **Run** tab: the dev server and tests get the variables as environment
  variables, without a `.env` file.
- Ask an agent: "Use run_with_env to run
  `python3 -c "import os; print(os.environ['LUNCHBOX_API_KEY'])"`" (uses
  Claude). An approval card shows the command first; the output comes back
  as `[secret:LUNCHBOX_API_KEY]`, so the agent never sees the value.
- If a value ever shows up in a conversation or a diff, Monaged masks it,
  says so, and won't merge or push commits that contain it.

---

To start over, remove these in WSL, then Ctrl K → Open the playground.
Monaged rebuilds it; the project stays and its old agents are archived.

```bash
rm -rf ~/code/monaged-playground ~/code/.monaged/monaged-playground ~/code/.remotes/monaged-playground.git
```
