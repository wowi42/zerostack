use crate::config::Config;
use crate::config::types::CustomProviderConfig;
use compact_str::CompactString;
use std::collections::HashMap;

fn custom_provider(provider_type: &str) -> CustomProviderConfig {
    CustomProviderConfig {
        provider_type: CompactString::new(provider_type),
        base_url: "https://gateway.example.com".to_string(),
        api_key_env: None,
        danger_accept_invalid_certs: None,
        api_style: None,
        headers: HashMap::new(),
        timeout_secs: None,
        model: None,
    }
}

#[test]
fn is_anthropic_native_builtin_providers() {
    let cfg = Config::default();
    assert!(cfg.is_anthropic_native("anthropic"));
    assert!(cfg.is_anthropic_native("Anthropic")); // case-insensitive
    for p in ["openai", "gemini", "google", "openrouter", "ollama"] {
        assert!(!cfg.is_anthropic_native(p), "{p} is not anthropic-native");
    }
}

#[test]
fn is_anthropic_native_resolves_custom_provider_type() {
    // A custom gateway named anything but routing through the Anthropic-native
    // protocol must be treated as anthropic-native (so cache fields are added),
    // while an OpenAI-style gateway must not.
    let mut providers = HashMap::new();
    providers.insert("my-claude-proxy".to_string(), custom_provider("anthropic"));
    providers.insert("my-oai-gateway".to_string(), custom_provider("openai"));
    let cfg = Config {
        custom_providers: Some(providers),
        ..Config::default()
    };
    assert!(cfg.is_anthropic_native("my-claude-proxy"));
    assert!(!cfg.is_anthropic_native("my-oai-gateway"));
    // Unknown name with no custom entry falls back to the literal kind.
    assert!(!cfg.is_anthropic_native("totally-unknown"));
}

#[test]
fn mid_turn_threshold_unset_by_default() {
    let cfg = Config::default();
    assert_eq!(cfg.resolve_mid_turn_compact_threshold(), None);
}

#[test]
fn mid_turn_threshold_valid_value_passes_through() {
    let cfg = Config {
        mid_turn_compact_threshold: Some(0.80),
        ..Config::default()
    };
    assert_eq!(cfg.resolve_mid_turn_compact_threshold(), Some(0.80));
}

#[test]
fn mid_turn_threshold_upper_bound_inclusive() {
    let cfg = Config {
        mid_turn_compact_threshold: Some(1.0),
        ..Config::default()
    };
    assert_eq!(cfg.resolve_mid_turn_compact_threshold(), Some(1.0));
}

#[test]
fn mid_turn_threshold_out_of_range_treated_as_unset() {
    // Zero would compact constantly; negatives and >1 are nonsense. All map to
    // "unset" so a misconfigured value silently disables the feature rather
    // than wedging the agent.
    for bad in [0.0, -0.1, 1.5, 2.0] {
        let cfg = Config {
            mid_turn_compact_threshold: Some(bad),
            ..Config::default()
        };
        assert_eq!(
            cfg.resolve_mid_turn_compact_threshold(),
            None,
            "threshold {bad} should be treated as unset"
        );
    }
}

#[test]
fn compact_enabled_default_true() {
    // Master switch defaults on; mid-turn compaction layers on top of it.
    assert!(Config::default().resolve_compact_enabled());
}

#[test]
fn show_reasoning_defaults_on() {
    assert!(Config::default().resolve_show_reasoning());
}

#[test]
fn show_reasoning_can_be_disabled() {
    let cfg = Config {
        show_reasoning: Some(false),
        ..Config::default()
    };
    assert!(!cfg.resolve_show_reasoning());
}

#[test]
fn context_exhausted_report_math() {
    // window 20000, threshold 0.80 -> ceiling 16000.
    // prompt 18000 -> 90% of window, overflow 18000 - 16000 = 2000.
    let lines = crate::ui::context_exhausted_report(18_000, 0.80, 20_000, 8_192, 6_000);
    let joined = lines.join("\n");
    assert!(
        joined.contains("context window .............. 20000 tokens"),
        "{joined}"
    );
    assert!(joined.contains("16000 tokens  (80% of window)"), "{joined}");
    assert!(joined.contains("18000 tokens  (90% of window)"), "{joined}");
    assert!(
        joined.contains("overflow above ceiling ...... 2000 tokens"),
        "{joined}"
    );
    assert!(
        joined.contains("reserved for response ....... 8192 tokens"),
        "{joined}"
    );
    assert!(
        joined.contains("kept-recent budget .......... 6000 tokens"),
        "{joined}"
    );
    // Guidance references the actual pressure and the floor the KV cache must hold.
    assert!(
        joined.contains("raise mid_turn_compact_threshold above 90%"),
        "{joined}"
    );
    assert!(joined.contains("hold 18000+ tokens"), "{joined}");
}

#[test]
fn catalog_context_window_reads_known_model() {
    // deepseek-v4-pro is a 1M-context model in the baked openrouter catalog.
    assert_eq!(
        Config::catalog_context_window("openrouter", "deepseek/deepseek-v4-pro"),
        Some(1_048_576)
    );
}

#[test]
fn catalog_context_window_none_for_unknown() {
    assert!(Config::catalog_context_window("openrouter", "no/such-model").is_none());
    // Providers without a baked catalog (custom gateways, ollama) return None.
    assert!(Config::catalog_context_window("ollama", "llama3.1").is_none());
}

#[test]
fn resolve_context_window_prefers_config_pin_over_catalog() {
    let cfg: Config = serde_json::from_str(r#"{ "context_window": 128000 }"#).unwrap();
    assert_eq!(
        cfg.resolve_context_window("openrouter", "deepseek/deepseek-v4-pro"),
        128_000
    );
    // Without a pin, the catalog's 1M wins.
    let cfg = Config::default();
    assert_eq!(
        cfg.resolve_context_window("openrouter", "deepseek/deepseek-v4-pro"),
        1_048_576
    );
}
