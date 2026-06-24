use crate::session::Session;
use crate::ui::status::StatusLine;

fn render(session: &Session) -> String {
    StatusLine::render(session, false, 0, None, None, None, None, 0.0, 0, 0).0
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
