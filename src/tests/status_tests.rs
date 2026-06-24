use crate::config::Config;
use crate::session::Session;
use crate::ui::status::StatusLine;

fn render(session: &Session) -> String {
    StatusLine::render(session, false, 0, None, None, None, None, 0.0, 0, 0).0
}

#[test]
fn cost_hidden_by_default_when_zero() {
    let session = Session::new("openrouter", "test-model", 128_000);
    assert!(!session.show_cost_always);
    assert!(
        !render(&session).contains('$'),
        "zero cost should be hidden by default"
    );
}

#[test]
fn cost_shown_when_always_flag_set_even_at_zero() {
    let mut session = Session::new("openrouter", "test-model", 128_000);
    session.show_cost_always = true;
    assert!(render(&session).contains("$0.0000"));
}

#[test]
fn cost_shown_when_nonzero_regardless_of_flag() {
    let mut session = Session::new("openrouter", "test-model", 128_000);
    session.total_cost = 0.1234;
    assert!(render(&session).contains("$0.1234"));
}

#[test]
fn resolve_show_cost_always_defaults_false() {
    let cfg = Config::default();
    assert!(!cfg.resolve_show_cost_always());
}

#[test]
fn resolve_show_cost_always_reads_config() {
    let cfg: Config = serde_json::from_str(r#"{ "show_cost_always": true }"#).unwrap();
    assert!(cfg.resolve_show_cost_always());
}

#[test]
fn footer_shows_branch_when_set() {
    let mut session = Session::new("openrouter", "test-model", 1_048_576);
    session.git_branch = Some("feat/footer-fields".into());
    let s = render(&session);
    assert!(s.contains("(feat/footer-fields)"), "{s}");
}

#[test]
fn footer_hides_branch_when_unset() {
    let session = Session::new("openrouter", "test-model", 1_048_576);
    assert!(!render(&session).contains('('));
}

#[test]
fn footer_shows_context_size_and_max_and_model() {
    let session = Session::new("openrouter", "deepseek/deepseek-v4-pro", 1_048_576);
    let s = render(&session);
    assert!(s.contains("ctx "), "should show context segment: {s}");
    assert!(s.contains("/1.0M"), "should show max context: {s}");
    assert!(
        s.contains("deepseek/deepseek-v4-pro"),
        "should show model: {s}"
    );
}
