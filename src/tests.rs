//! Unit tests for the pure logic: diff parsing, Markdown parsing, scoring.

use serde_json::json;

use crate::diff::{self, LineKind};
use crate::pipeline::{self, Directive, Gate, Role, Segment, Stage};
use crate::markdown::{self, Block};
use crate::review::{self, Checks, Issue};

const DIFF: &str = "\
diff --git a/src/app.rs b/src/app.rs
index 1111111..2222222 100644
--- a/src/app.rs
+++ b/src/app.rs
@@ -10,3 +10,4 @@ fn main() {
     let a = 1;
-    let b = 2;
+    let b = 3;
+    --- not a header, a removed-looking added line
     run(a, b);
diff --git a/frontend/README.md b/frontend/README.md
new file mode 100644
index 0000000..3333333
--- /dev/null
+++ b/frontend/README.md
@@ -0,0 +1,2 @@
+# Reservation form
+Fields: car, dates.
";

#[test]
fn parses_modified_and_new_files() {
    let files = diff::parse(DIFF);
    assert_eq!(files.len(), 2);

    let app = &files[0];
    assert_eq!(app.path, "src/app.rs");
    assert_eq!(app.status, "modified");
    assert_eq!((app.added, app.removed), (2, 1));
    let removed = app.lines.iter().find(|l| l.kind == LineKind::Remove).unwrap();
    assert_eq!(removed.old_no, Some(11));
    let added: Vec<_> = app.lines.iter().filter(|l| l.kind == LineKind::Add).collect();
    assert_eq!(added[0].new_no, Some(11));
    assert_eq!(added[1].text, "    --- not a header, a removed-looking added line");

    let readme = &files[1];
    assert_eq!(readme.path, "frontend/README.md");
    assert_eq!(readme.status, "added");
    assert_eq!((readme.added, readme.removed), (2, 0));
    assert_eq!(diff::diffstat(&files).lines().count(), 2);
}

#[test]
fn markdown_blocks_and_styles() {
    let blocks = markdown::parse(
        "# Title\n\nSome **bold** and *it* and `code`.\n\n- one\n- two\n  - nested\n\n1. first\n2. second\n\n```rust\nfn x() {}\n```\n",
    );
    let kinds: Vec<&str> = blocks
        .iter()
        .map(|b| match b {
            Block::Heading(..) => "h",
            Block::Paragraph(_) => "p",
            Block::Item { .. } => "li",
            Block::Quote(_) => "q",
            Block::Code(..) => "code",
            Block::Rule => "hr",
        })
        .collect();
    assert_eq!(kinds, ["h", "p", "li", "li", "li", "li", "li", "code"]);

    let Block::Paragraph(p) = &blocks[1] else { panic!() };
    assert_eq!(p.text, "Some bold and it and code.");
    let styled: Vec<&str> = p.styles.iter().map(|(r, _)| &p.text[r.clone()]).collect();
    assert_eq!(styled, ["bold", "it", "code"]);

    let markers: Vec<(usize, &str)> = blocks
        .iter()
        .filter_map(|b| if let Block::Item { depth, marker, .. } = b { Some((*depth, marker.as_str())) } else { None })
        .collect();
    assert_eq!(markers, [(0, "•"), (0, "•"), (1, "•"), (0, "1."), (0, "2.")]);
    let Block::Item { content, .. } = &blocks[3] else { panic!() };
    assert_eq!(content.text, "two");
}

#[test]
fn splits_reply_into_text_and_directives() {
    let reply = "I looked around.\n\n```monaged-question\n{\"question\": \"Which DB?\", \"why\": \"schema\", \"options\": [\"Postgres\", \"SQLite\"], \"recommended\": 1}\n```\n\nAnd:\n```monaged-research\n{\"summary\": \"ok\"}\n```\n```monaged-plan\nnot json\n```\n";
    let segments = pipeline::segments(reply);
    assert_eq!(segments.len(), 5);
    assert!(matches!(&segments[0], Segment::Text(t) if t.contains("I looked around")));
    let directives = pipeline::directives(reply);
    match &directives[0] {
        Directive::Question(q) => {
            assert_eq!(q.question, "Which DB?");
            assert_eq!(q.options, ["Postgres", "SQLite"]);
            assert_eq!(q.recommended, Some(1));
        }
        other => panic!("expected a question, got {other:?}"),
    }
    assert!(matches!(&directives[1], Directive::Research(v) if v["summary"] == "ok"));
    assert!(matches!(&directives[2], Directive::Invalid { kind, .. } if kind == "plan"));
    // A block that is still streaming (no closing fence) stays text.
    assert!(pipeline::directives("```monaged-plan\n{\"title\": \"x\"").is_empty());
}

#[test]
fn roles_and_stages_round_trip() {
    for role in [Role::Free, Role::Researcher, Role::Verifier(2), Role::Planner, Role::Builder("api".into()), Role::Integrator] {
        assert_eq!(Role::from_db(&role.to_db()), role);
    }
    for stage in pipeline::STAGES {
        assert_eq!(Stage::parse(stage.as_str()), stage);
    }
}

#[test]
fn gates_follow_the_users_rules() {
    let small = json!({"size": "small", "confidence": 5, "risk_flags": []});
    let migration = json!({"size": "small", "confidence": 5, "risk_flags": ["db_migration"]});
    let large = json!({"size": "large", "confidence": 5, "risk_flags": []});
    let unsure = json!({"size": "small", "confidence": 3, "risk_flags": []});
    assert!(matches!(pipeline::plan_gate(&small, true, true), Gate::Auto(_)));
    assert!(matches!(pipeline::plan_gate(&small, true, false), Gate::Ask(_)), "auto mode off");
    assert!(matches!(pipeline::plan_gate(&small, false, true), Gate::Ask(_)), "project always asks");
    assert!(matches!(pipeline::plan_gate(&migration, true, true), Gate::Ask(_)), "risky");
    assert!(matches!(pipeline::plan_gate(&large, true, true), Gate::Ask(_)), "large");
    assert!(matches!(pipeline::plan_gate(&unsure, true, true), Gate::Ask(_)), "low confidence");

    let ok = [json!({"verdict": "confirmed"}), json!({"verdict": "minor_issues"})];
    let bad = [json!({"verdict": "confirmed"}), json!({"verdict": "major_issues"})];
    assert!(matches!(pipeline::decide_gate(&ok, true), Gate::Auto(_)));
    assert!(matches!(pipeline::decide_gate(&ok, false), Gate::Ask(_)));
    assert!(matches!(pipeline::decide_gate(&bad, true), Gate::Ask(_)));
}

#[test]
fn workstreams_are_named_and_capped() {
    let many: Vec<_> = (0..14).map(|i| json!({"name": format!("Part {i}!")})).collect();
    let streams = pipeline::workstreams(&json!({ "workstreams": many }));
    assert_eq!(streams.len(), pipeline::MAX_BUILDERS);
    assert_eq!(streams[0].name, "part-0");
}

#[test]
fn parses_git_remotes() {
    use crate::hosting::{Provider, parse_remote};
    let gh = parse_remote("git@github.com:me/private-app.git").unwrap();
    assert_eq!((gh.provider, gh.host.as_str(), gh.slug.as_str()), (Some(Provider::GitHub), "github.com", "me/private-app"));
    let gl = parse_remote("https://gitlab.com/work-group/hr/hr-app.git").unwrap();
    assert_eq!((gl.provider, gl.slug.as_str()), (Some(Provider::GitLab), "work-group/hr/hr-app"));
    let selfhosted = parse_remote("ssh://git@gitlab.company.nl:2222/team/api.git").unwrap();
    assert_eq!((selfhosted.provider, selfhosted.host.as_str(), selfhosted.slug.as_str()), (Some(Provider::GitLab), "gitlab.company.nl", "team/api"));
    let token = parse_remote("https://user:token@github.com/me/repo").unwrap();
    assert_eq!(token.slug, "me/repo");
    assert_eq!(parse_remote("https://gitea.local/me/repo").unwrap().provider, None);
    assert!(parse_remote("not a url").is_none());
}

#[test]
fn branch_chain_and_policies_follow_the_users_rules() {
    use crate::git::BranchState;
    use crate::merge_ui::{Policy, chain_from, default_policy};
    let state = |names: &[&str]| BranchState { branches: names.iter().map(|s| s.to_string()).collect(), ..Default::default() };

    let full = chain_from(&state(&["main", "develop", "acceptance", "feature/x"]), "main");
    assert_eq!(full, ["develop", "acceptance", "main"]);
    assert_eq!(default_policy(&full, "develop"), Policy::Auto);
    assert_eq!(default_policy(&full, "acceptance"), Policy::Ask);
    assert_eq!(default_policy(&full, "main"), Policy::Confirm);

    // Only main: features merge straight into it, and that asks.
    let only_main = chain_from(&state(&["main", "feature/x"]), "main");
    assert_eq!(only_main, ["main"]);
    assert_eq!(default_policy(&only_main, "main"), Policy::Ask);

    assert_eq!(chain_from(&state(&["master", "develop"]), "master"), ["develop", "master"]);
    assert_eq!(chain_from(&state(&["trunk"]), "trunk"), ["trunk"]);
}

fn issue(severity: &str) -> Issue {
    Issue { severity: severity.into(), file: "f".into(), problem: "p".into() }
}

#[test]
fn score_is_capped_by_checks_and_verdict() {
    // Tests pass, reviewer happy: full marks.
    assert_eq!(review::score(&Checks::Passed("cargo test".into()), "works", &[]).0, 5);
    // No tests: never above 4, however happy the reviewer is.
    assert_eq!(review::score(&Checks::NotFound, "works", &[]).0, 4);
    // Failing tests cap at 2.
    assert_eq!(review::score(&Checks::Failed { command: "t".into(), tail: String::new() }, "works", &[]).0, 2);
    // Serious issues pull the score down.
    assert_eq!(review::score(&Checks::Passed("t".into()), "works", &[issue("high"), issue("high")]).0, 2);
    // Broken verdict caps at 2; floor is 1.
    let many: Vec<Issue> = (0..5).map(|_| issue("high")).collect();
    assert_eq!(review::score(&Checks::NotFound, "broken", &many).0, 1);
    // Partly done caps at 3.
    assert_eq!(review::score(&Checks::Passed("t".into()), "partly", &[]).0, 3);
}

#[test]
fn saved_checks_read_back_for_scoring() {
    let all = [
        Checks::NotFound,
        Checks::Unavailable("no tests configured".into()),
        Checks::Passed("cargo test".into()),
        Checks::Failed { command: "npm test".into(), tail: "1 failing\n  at foo.js:3".into() },
        Checks::FailedBefore { command: "cargo test".into(), tail: "test a ... FAILED".into() },
    ];
    for checks in all {
        let back = Checks::from_description(&checks.describe());
        assert_eq!(back.describe(), checks.describe());
    }
}

fn confidence(checks: &Checks, verdict: &str, issues: Vec<Issue>) -> review::Confidence {
    let (score, mut reasons) = review::score(checks, verdict, &issues);
    reasons.push("Reviewer: the summary".into());
    review::Confidence { score, verdict: verdict.into(), reasons, issues, checks: checks.describe() }
}

#[test]
fn ruling_out_findings_scores_again() {
    let passed = Checks::Passed("cargo test".into());
    let mut c = confidence(&passed, "broken", vec![
        Issue { severity: "high".into(), file: "a.rs".into(), problem: "calls a missing fn".into() },
        Issue { severity: "low".into(), file: "b.rs".into(), problem: "nit".into() },
    ]);
    assert_eq!(c.score, 2);
    let gone = review::rule_out(&mut c, 0).unwrap();
    assert_eq!(gone.problem, "calls a missing fn");
    // No serious finding left: "broken" no longer stands, but it isn't "works" either.
    assert_eq!(c.verdict, "unclear");
    assert_eq!(c.score, 4);
    assert!(c.reasons.iter().any(|r| r == "– You ruled out 1 finding; review again to confirm"));
    assert!(c.reasons.iter().any(|r| r == "Reviewer: the summary"));
    review::rule_out(&mut c, 0).unwrap();
    assert!(c.issues.is_empty());
    assert!(c.reasons.iter().any(|r| r.starts_with("– You ruled out 2 findings")));
    assert_eq!(c.reasons.iter().filter(|r| r.starts_with("– You ruled out")).count(), 1);
    assert_eq!(c.reasons.iter().filter(|r| r.starts_with("Reviewer:")).count(), 1);
    assert!(review::rule_out(&mut c, 0).is_none());
    // Failing tests still cap the score, whatever you rule out.
    let failed = Checks::Failed { command: "t".into(), tail: "boom".into() };
    let mut c = confidence(&failed, "works", vec![issue("high")]);
    review::rule_out(&mut c, 0);
    assert_eq!(c.score, 2);
}

#[test]
fn the_fix_message_lists_findings_and_failing_tests() {
    let passed = Checks::Passed("cargo test".into());
    assert!(review::fix_prompt(&confidence(&passed, "works", vec![])).is_none());
    let failed = Checks::Failed { command: "cargo test".into(), tail: "test x ... FAILED".into() };
    let text = review::fix_prompt(&confidence(&failed, "partly", vec![
        Issue { severity: "medium".into(), file: "src/a.rs".into(), problem: "off by one".into() },
        Issue { severity: "low".into(), file: String::new(), problem: "no docs".into() },
    ]))
    .unwrap();
    assert!(text.contains("isn't a real problem, say so"));
    assert!(text.contains("The tests failed (`cargo test`)"));
    assert!(text.contains("test x ... FAILED"));
    assert!(text.contains("1. [medium] src/a.rs: off by one"));
    assert!(text.contains("2. [low] no docs"));
}

#[test]
fn the_reviewer_gets_the_projects_context() {
    let context = review::ReviewContext {
        task: "Add a login page".into(),
        follow_ups: vec!["Use the existing session helper".into()],
        report: "I added login.rs and wired it up.".into(),
        knowledge: "## CLAUDE.md\nWe never use unwrap in handlers.".into(),
        files: vec![("src/login.rs".into(), "fn login() {}".into())],
        files_left_out: 2,
        settled: vec![review::Settled { file: "src/db.rs".into(), problem: "no index".into(), reason: "table has 10 rows".into() }],
    };
    let (prompt, note) = review::review_prompt(&context, "+fn login() {}", &Checks::NotFound);
    assert!(note.is_empty());
    let order = ["Add a login page", "Use the existing session helper", "We never use unwrap", "I added login.rs", "table has 10 rows", "No automated checks", "+fn login() {}", "--- src/login.rs ---", "2 more changed file(s)"];
    let mut at = 0;
    for part in order {
        let found = prompt[at..].find(part).unwrap_or_else(|| panic!("{part:?} missing or out of order"));
        at += found;
    }
    // Nothing extra when there's no extra context.
    let bare = review::ReviewContext { task: "t".into(), ..Default::default() };
    let (prompt, _) = review::review_prompt(&bare, "+x", &Checks::NotFound);
    assert!(!prompt.contains("later messages") && !prompt.contains("project's notes") && !prompt.contains("ruled out") && !prompt.contains("as they are now"));
}

#[test]
fn review_context_is_read_between_markers() {
    let script = review::context_script("/home/me/wt", Some("~/.monaged/app/CLAUDE.md"), &["src/a b.rs".into()], "M");
    assert!(script.contains("emit CLAUDE.md knowledge:CLAUDE.md"));
    assert!(script.contains("\"$HOME\"/"));
    assert!(script.contains("'src/a b.rs'"));
    let out = "noise\nM_BEGIN knowledge:CLAUDE.md\nRules\nmore\n\nM_END\nM_BEGIN file:src/a b.rs\nfn a() {}\nM_END\n";
    let parsed = review::parse_context(out, "M");
    assert_eq!(parsed, vec![("knowledge:CLAUDE.md".to_string(), "Rules\nmore".to_string()), ("file:src/a b.rs".to_string(), "fn a() {}".to_string())]);
    let settled = vec![review::Settled { file: "f".into(), problem: "p".into(), reason: String::new() }];
    assert_eq!(review::settled_from_json(&review::settled_to_json(&settled).to_string()), settled);
}

#[test]
fn failures_that_were_there_before_dont_count() {
    // unittest: the same failing test on both runs, timings and counts differ.
    let before = "FAIL: test_coupon_applies_to_the_whole_order (tests.test_orders.OrderTests.test_coupon_applies_to_the_whole_order)\nFAILED (failures=1)";
    let now = "FAIL: test_coupon_applies_to_the_whole_order (tests.test_orders.OrderTests.test_coupon_applies_to_the_whole_order)\nFAILED (failures=1)";
    assert!(review::only_failed_before(now, before));
    // A new failure is the agent's.
    let now2 = format!("{now}\nFAIL: test_history_is_newest_first (tests.test_history.HistoryTests.test_history_is_newest_first)");
    assert!(!review::only_failed_before(&now2, before));
    // pytest: messages after " - " and cargo/go timings are left out.
    assert!(review::only_failed_before("FAILED tests/test_a.py::test_x - AssertionError: 1 != 2", "FAILED tests/test_a.py::test_x - AssertionError: 3 != 4"));
    assert!(review::only_failed_before("--- FAIL: TestParse (0.01s)", "--- FAIL: TestParse (0.20s)"));
    assert!(review::only_failed_before("test parser::tests::empty ... FAILED", "test parser::tests::empty ... FAILED\ntest other ... FAILED"));
    // Nothing recognisable to compare: it counts as failing (the safe side).
    assert!(!review::only_failed_before("Error: cannot find module 'x'", "Error: cannot find module 'x'"));
    // Fixed a failure that was there before and broke nothing: fine.
    assert!(review::only_failed_before("FAIL: a (t.T.a)", "FAIL: a (t.T.a)\nFAIL: b (t.T.b)"));
    // Scored as at most 4, and read back from a saved review.
    let checks = Checks::FailedBefore { command: "python3 -m unittest".into(), tail: "FAIL: a".into() };
    assert_eq!(review::score(&checks, "works", &[]).0, 4);
    assert_eq!(Checks::from_description(&checks.describe()).describe(), checks.describe());
}

#[test]
fn the_quality_gate_sends_work_back_then_asks() {
    use crate::pipeline::{GateStep, MAX_FIX_ROUNDS, QualityGate};
    let mut gate = QualityGate::default();
    gate.submit("build-api");
    assert_eq!(gate.pending, vec!["build-api".to_string()]);
    // At the bar: accepted.
    assert_eq!(gate.step("build-api", Some(4), "", 4), GateStep::Accept);
    assert_eq!(gate.step("build-api", Some(5), "", 4), GateStep::Accept);
    // Below it: fixed automatically, round by round.
    assert_eq!(gate.step("build-api", Some(2), "", 4), GateStep::Fix(1));
    assert_eq!(gate.fix("build-api", true), 1);
    assert!(gate.pending.is_empty() && gate.fixing == vec!["build-api".to_string()]);
    gate.submit("build-api");
    assert!(gate.fixing.is_empty());
    assert_eq!(gate.step("build-api", Some(3), "", 4), GateStep::Fix(2));
    gate.fix("build-api", true);
    gate.submit("build-api");
    // Out of rounds: it waits for you.
    assert_eq!(MAX_FIX_ROUNDS, 2);
    let GateStep::Hold(why) = gate.step("build-api", Some(1), "", 4) else { panic!("should hold") };
    assert!(why.contains("1/5 after 2 fix rounds") && why.contains("4/5"));
    gate.hold("build-api", why);
    assert!(gate.pending.is_empty() && gate.held_reason("build-api").is_some());
    // Fixing again by hand doesn't use up a round.
    gate.fix("build-api", false);
    assert_eq!(gate.rounds_for("build-api"), 2);
    assert!(gate.held.is_empty());
    // Work that couldn't be scored waits for you, whatever the rounds.
    assert_eq!(gate.step("build-ui", None, "couldn't be reviewed", 4), GateStep::Hold("couldn't be reviewed".into()));
    // A lower bar lets a 3 through.
    assert_eq!(gate.step("build-ui", Some(3), "", 3), GateStep::Accept);
    // It survives a restart.
    gate.hold("build-ui", "why".into());
    assert_eq!(QualityGate::from_json(&gate.to_json().to_string()), gate);
    assert_eq!(QualityGate::from_json("not json"), QualityGate::default());
}

#[test]
fn the_lead_gets_its_own_tools() {
    let args: Vec<String> = ["monaged.exe", "--mcp-relay", "--port", "4000", "--token", "t", "--agent", "7", "--lead"].iter().map(|s| s.to_string()).collect();
    let relay = crate::coord::parse_relay_args(&args).unwrap();
    assert!(relay.lead && relay.agent == 7);
    let plain = crate::coord::parse_relay_args(&args[..8]).unwrap();
    assert!(!plain.lead);
    let names: Vec<String> = crate::lead::tools().as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap().to_string()).collect();
    assert_eq!(names, ["team_status", "message_agent", "start_agent", "fix_findings", "review_agent"]);
    assert!(names.iter().all(|n| crate::lead::is_tool(n)));
    // The everyday tools stay apart from the lead's.
    let base: Vec<String> = crate::coord::tool_list().as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap().to_string()).collect();
    assert!(base.iter().all(|n| !crate::lead::is_tool(n)));
    // The lead is a role of its own, stored and read back.
    assert_eq!(Role::from_db(&Role::Lead.to_db()), Role::Lead);
    assert_eq!(Role::Lead.result_block(), None);
    let brief = crate::lead::brief("Order history", "feature/order-history", Some("Keep every order"));
    assert!(brief.starts_with(pipeline::BRIEF_HEADER));
    assert!(brief.contains("Keep every order") && brief.contains("team_status") && brief.contains(crate::lead::UPDATE_PREFIX));
    assert!(crate::lead::brief("x", "b", None).contains("Nothing yet"));
}

#[test]
fn coordination_paths_overlap() {
    use crate::coord::{glob_match, normalize_path, overlaps};
    assert_eq!(normalize_path(r"./src\api/"), "src/api");
    assert!(glob_match("src/api/**", "src/api/routes/cars.ts"));
    assert!(glob_match("src/**/*.test.ts", "src/a/b/c.test.ts"));
    assert!(glob_match("src/*.rs", "src/main.rs"));
    assert!(!glob_match("src/*.rs", "src/ui/main.rs"));
    // A file inside an owned glob, a directory above it, and an unrelated file.
    assert!(overlaps("src/api/routes.ts", "src/api/**"));
    assert!(overlaps("src", "src/api/**"));
    assert!(!overlaps("src/ui/App.tsx", "src/api/**"));
    // Directories and plain files.
    assert!(overlaps("src/api", "src/api/routes.ts"));
    assert!(!overlaps("src/api2/x.ts", "src/api"));
    // Two globs in the same or different trees.
    assert!(overlaps("src/**", "src/api/*.ts"));
    assert!(!overlaps("web/**", "server/**"));
}

#[test]
fn coordination_relay_and_answers() {
    use crate::coord::{ask_marker, ask_message, parse_relay_args, windows_to_wsl_path};
    let args: Vec<String> = ["monaged.exe", "--mcp-relay", "--port", "51234", "--token", "abc", "--agent", "7"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let relay = parse_relay_args(&args).expect("relay args");
    assert_eq!((relay.port, relay.token.as_str(), relay.agent), (51234, "abc", 7));
    assert!(parse_relay_args(&["monaged.exe".to_string()]).is_none());
    assert_eq!(windows_to_wsl_path(r"O:\Projects\app\target\debug\monaged.exe"), "/mnt/o/Projects/app/target/debug/monaged.exe");
    assert_eq!(windows_to_wsl_path(r"\\?\C:\x\m.exe"), "/mnt/c/x/m.exe");
    let message = ask_message(3, "builder-ui", "Which field holds the car id?");
    assert!(message.starts_with(&ask_marker(3)));
    let reply = "Sure.\n```monaged-answer\n{\"answer\": \"carId (number)\"}\n```";
    assert!(matches!(pipeline::directives(reply).as_slice(), [Directive::Answer(v)] if v["answer"] == "carId (number)"));
}

#[test]
fn usage_rate_limit_parsing() {
    use crate::usage::parse_rate_limit;
    let v = json!({
        "status": "allowed", "resetsAt": 1795147200, "rateLimitType": "seven_day",
        "unifiedWindows": {
            "seven_day": { "utilization": 0.03, "resetsAt": 1795147200, "status": "allowed" },
            "seven_day_overage_included": { "utilization": 0.02, "resetsAt": 1795147200, "status": "allowed" }
        }
    });
    let plan = parse_rate_limit(&v).expect("parses");
    let week = plan.week.clone().expect("week");
    assert_eq!((week.utilization, week.resets_at), (0.03, 1795147200));
    assert!(plan.five_hour.is_none() && plan.limited().is_none());
    let limited = json!({ "unifiedWindows": { "five_hour": { "utilization": 1.0, "resetsAt": 5, "status": "rejected" } } });
    assert_eq!(parse_rate_limit(&limited).unwrap().limited(), Some(5));
    assert!(parse_rate_limit(&json!(null)).is_none());
}

#[test]
fn usage_meter_attributes_and_calibrates() {
    use crate::usage::{DEFAULT_SHARE_PER_USD, Meter, cost_delta};
    let mut m = Meter::default();
    assert_eq!(m.reading(0.03, 1000), 0.0); // first reading starts the week
    // Busy the whole time: the whole increase is Monaged's, and it calibrates.
    m.add_cost(4.0);
    assert!((m.reading(0.05, 1000) - 0.02).abs() < 1e-9);
    assert!((m.share - 0.02).abs() < 1e-9);
    assert!(!m.calibrated() || m.share_per_usd() > 0.0);
    m.add_cost(1.0);
    assert!((m.reading(0.06, 1000) - 0.01).abs() < 1e-9);
    assert!(m.calibrated());
    let k = m.share_per_usd(); // (0.03 + 0.005) / 5
    assert!((k - 0.007).abs() < 1e-9);
    // Idle in between (you used Claude yourself): only what Monaged's cost explains counts.
    m.add_cost(1.0);
    m.went_idle();
    let counted = m.reading(0.20, 1000);
    assert!((counted - k).abs() < 1e-9, "counted {counted}");
    // Live share includes the cost since the last reading.
    m.add_cost(2.0);
    assert!((m.live_share() - (m.share + 2.0 * k)).abs() < 1e-9);
    // A new week starts from zero but keeps the calibration.
    assert_eq!(m.reading(0.01, 2000), 0.0);
    assert_eq!((m.share, m.pending_cost), (0.0, 0.0));
    assert!((m.share_per_usd() - k).abs() < 1e-9);
    // Rolling over without a reading.
    assert!(m.roll_over(2000 + 10));
    assert_eq!(m.week_key, 2000 + crate::usage::WEEK_SECS);
    // Restored from disk: idle gap is assumed.
    let restored = Meter::from_json(&m.to_json());
    assert!(restored.idle_gap && restored.week_key == m.week_key);
    assert_eq!(Meter::default().share_per_usd(), DEFAULT_SHARE_PER_USD);
    // Session cost is cumulative; a restart counts from zero.
    assert_eq!(cost_delta(None, 0.5), 0.5);
    assert!((cost_delta(Some(0.5), 0.75) - 0.25).abs() < 1e-9);
    assert_eq!(cost_delta(Some(0.5), 0.1), 0.1);
}

#[test]
fn usage_budget_and_estimates() {
    use crate::usage::{Block, Budget, Meter, PlanUsage, Window, check, estimate_cost, pretty_model};
    let mut meter = Meter { week_key: 9, share: 0.10, ..Meter::default() };
    let week = |u: f64, status: &str| PlanUsage {
        week: Some(Window { utilization: u, resets_at: 9, status: status.into() }),
        five_hour: None,
        status: "allowed".into(),
    };
    let budget = Budget::default(); // 25% for Monaged, pause at 90% overall
    assert_eq!(check(budget, &meter, &week(0.5, "allowed")), None);
    meter.share = 0.26;
    assert!(matches!(check(budget, &meter, &week(0.5, "allowed")), Some(Block::Share { .. })));
    assert_eq!(check(Budget { share_pct: 100, ..budget }, &meter, &week(0.5, "allowed")), None);
    meter.share = 0.0;
    assert!(matches!(check(budget, &meter, &week(0.91, "allowed")), Some(Block::Ceiling { .. })));
    assert!(matches!(check(budget, &meter, &week(0.2, "rejected")), Some(Block::Limited { resets_at: 9 })));
    // Estimates: defaults first, then history by size.
    let (guess, n) = estimate_cost("small", 2, &[]);
    assert_eq!((guess, n), (10.0, 0));
    let history = vec![("small".to_string(), 2, 6.0), ("large".to_string(), 4, 100.0)];
    assert_eq!(estimate_cost("small", 3, &history), (9.0, 1));
    assert_eq!(pretty_model("claude-sonnet-5-5"), "Sonnet 5.5");
    assert_eq!(pretty_model("claude-haiku-4-5-20251001"), "Haiku 4.5");
    assert_eq!(pretty_model("claude-fable-5-2"), "Fable 5.2");
}
