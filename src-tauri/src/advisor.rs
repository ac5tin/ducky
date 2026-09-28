//! The advisor: a conversation-scoped second-opinion model the executor can
//! consult. Selection resolution, request assembly and the pinned copy live
//! here; the provider call and the tool card live in `agent.rs`.

use crate::config::{AppSettings, ConversationMeta, EffortLevel, ProviderConfig};
use crate::providers::Msg;

pub const ADVISOR_SYSTEM_PROMPT: &str = r#"You are the advisor in an advisor-strategy pattern. An executor model is
running a task end to end — calling tools, reading results, iterating. It has
stopped to ask you for guidance. You read the shared conversation and answer
with exactly one of: a plan (concrete next steps the executor should take), a
correction (the executor is on a wrong path — say where, and what to do
instead), or a stop signal (the work should halt and the user should be
asked). You never call tools. You never produce user-facing prose. Be concise
and directive, and name files, functions and line numbers from the
conversation. Ground every claim in the conversation; when the conversation
does not settle the question, say what is missing rather than guessing. No
preamble, no apology, no meta-commentary about being an advisor."#;

pub const ADVISOR_ASK: &str =
    "Decide what the executor should do next from the conversation above and give it the guidance it needs.";

pub const ADVISOR_MAX_TOKENS: u32 = 2048;

pub fn build_messages(history: &[Msg], tool_names: &[String], executor_system: &str) -> Vec<Msg> {
    let joined = if tool_names.is_empty() {
        "none".to_owned()
    } else {
        tool_names.join(", ")
    };
    let system = format!(
        "{ADVISOR_SYSTEM_PROMPT}\n\nThe executor is running under this system prompt:\n<executor-system-prompt>\n{executor_system}\n</executor-system-prompt>\n\nTools the executor can call: {joined}"
    );

    let mut transcript = history.to_vec();
    // The consult can be batched with other calls: the assistant message then
    // still carries the in-flight `ducky__advisor` call while the earlier
    // calls' results are already in the history. Providers reject an assistant
    // `tool_use` without a matching `tool_result`, so strip every unanswered
    // call of the last assistant message that carries any.
    let last_calls = transcript.iter().rposition(
        |message| matches!(message, Msg::Assistant { tool_calls, .. } if !tool_calls.is_empty()),
    );
    if let Some(index) = last_calls {
        let answered: Vec<String> = transcript[index + 1..]
            .iter()
            .filter_map(|message| match message {
                Msg::ToolResult { call_id, .. } => Some(call_id.clone()),
                _ => None,
            })
            .collect();
        let mut left_empty = false;
        if let Msg::Assistant {
            text, tool_calls, ..
        } = &mut transcript[index]
        {
            tool_calls.retain(|call| answered.contains(&call.id));
            left_empty = text.is_empty() && tool_calls.is_empty();
        }
        if left_empty {
            transcript.remove(index);
        }
    }

    let mut messages = Vec::with_capacity(transcript.len() + 2);
    messages.push(Msg::System { text: system });
    messages.extend(transcript);
    messages.push(Msg::User {
        text: ADVISOR_ASK.into(),
        ts: None,
    });
    messages
}

pub fn err_no_model() -> String {
    "No advisor model is configured. Pick one in the chat header.".into()
}

pub fn err_failed(err: &str) -> String {
    format!("Advisor call failed: {err}")
}

pub fn err_cancelled() -> String {
    "Advisor call was cancelled before it completed.".into()
}

pub fn err_empty() -> String {
    "Advisor returned no text content.".into()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdvisorSpec {
    pub provider_id: String,
    pub model: String,
    pub effort: Option<EffortLevel>,
}

pub fn resolve(
    settings: &AppSettings,
    meta: &ConversationMeta,
    providers: &[ProviderConfig],
) -> Option<AdvisorSpec> {
    if !meta.advisor_enabled {
        return None;
    }

    let (provider_id, model, effort) = if let Some(provider_id) = &meta.advisor_provider_id {
        if !providers.iter().any(|provider| provider.id == *provider_id) {
            return None;
        }
        (
            provider_id,
            meta.advisor_model.as_deref().unwrap_or("").to_owned(),
            meta.advisor_effort,
        )
    } else {
        let provider_id = settings.advisor_provider_id.as_ref()?;
        if !providers.iter().any(|provider| provider.id == *provider_id) {
            return None;
        }
        (
            provider_id,
            settings.advisor_model.as_deref().unwrap_or("").to_owned(),
            settings.advisor_effort,
        )
    };

    Some(AdvisorSpec {
        provider_id: provider_id.clone(),
        model,
        effort,
    })
}

/// Validate one advisor pick. A disable is never refused: when `enabled` is
/// `false` the pick is accepted as passed, so a dangling provider can never
/// trap the switch on.
pub fn validate_selection(
    enabled: bool,
    provider_id: Option<&str>,
    model: Option<&str>,
    effort: Option<EffortLevel>,
    providers: &[ProviderConfig],
) -> Result<(), String> {
    if !enabled {
        return Ok(());
    }
    let provider_id = provider_id.filter(|id| !id.trim().is_empty());
    if let Some(provider_id) = provider_id {
        if !providers.iter().any(|provider| provider.id == provider_id) {
            return Err("That provider is no longer configured".into());
        }
    } else if model.is_some() || effort.is_some() {
        return Err("Pick a provider before setting a model".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::{Msg, ToolCall};

    fn provider(id: &str) -> ProviderConfig {
        ProviderConfig {
            id: id.into(),
            kind: "custom".into(),
            name: "Test".into(),
            base_url: "https://x".into(),
            api_type: crate::config::ApiType::OpenAi,
            default_model: None,
            models: vec!["m1".into(), "m2".into()],
            created_at: "t".into(),
        }
    }

    fn meta() -> ConversationMeta {
        ConversationMeta {
            id: "c1".into(),
            title: "test".into(),
            provider_id: "p1".into(),
            model: "m1".into(),
            effort: None,
            mcp_ids: None,
            mode: crate::config::AgentMode::Default,
            auto_readonly: false,
            advisor_enabled: false,
            advisor_provider_id: None,
            advisor_model: None,
            advisor_effort: None,
            working_dir: None,
            created_at: "t".into(),
            updated_at: "t".into(),
        }
    }

    #[test]
    fn resolve_off_when_disabled() {
        let settings = AppSettings {
            advisor_provider_id: Some("p1".into()),
            advisor_model: Some("m2".into()),
            ..Default::default()
        };
        assert!(resolve(&settings, &meta(), &[provider("p1")]).is_none());
    }

    #[test]
    fn resolve_uses_the_session_triple_when_set() {
        let settings = AppSettings {
            advisor_provider_id: Some("p1".into()),
            advisor_model: Some("m1".into()),
            advisor_effort: Some(EffortLevel::Low),
            working_dir: None,
            ..Default::default()
        };
        let mut m = meta();
        m.advisor_enabled = true;
        m.advisor_provider_id = Some("p2".into());
        m.advisor_model = Some("m2".into());
        m.advisor_effort = Some(EffortLevel::High);

        let spec = resolve(&settings, &m, &[provider("p1"), provider("p2")]).unwrap();
        assert_eq!(spec.provider_id, "p2");
        assert_eq!(spec.model, "m2");
        assert!(matches!(spec.effort, Some(EffortLevel::High)));
    }

    #[test]
    fn resolve_none_when_the_session_provider_is_gone() {
        let settings = AppSettings {
            advisor_provider_id: Some("p1".into()),
            ..Default::default()
        };
        let mut m = meta();
        m.advisor_enabled = true;
        m.advisor_provider_id = Some("gone".into());
        assert!(resolve(&settings, &m, &[provider("p1")]).is_none());
    }

    #[test]
    fn resolve_falls_back_to_the_settings_triple() {
        let settings = AppSettings {
            advisor_provider_id: Some("p1".into()),
            advisor_model: Some("m2".into()),
            advisor_effort: Some(EffortLevel::Medium),
            working_dir: None,
            ..Default::default()
        };
        let mut m = meta();
        m.advisor_enabled = true;
        m.advisor_effort = Some(EffortLevel::XHigh); // ignored: the triple is atomic

        let spec = resolve(&settings, &m, &[provider("p1")]).unwrap();
        assert_eq!(spec.provider_id, "p1");
        assert_eq!(spec.model, "m2");
        assert!(matches!(spec.effort, Some(EffortLevel::Medium)));
    }

    #[test]
    fn resolve_none_when_nothing_is_configured() {
        let mut m = meta();
        m.advisor_enabled = true;
        assert!(resolve(&AppSettings::default(), &m, &[provider("p1")]).is_none());
    }

    #[test]
    fn resolve_empty_model_means_the_provider_default() {
        let settings = AppSettings {
            advisor_provider_id: Some("p1".into()),
            ..Default::default()
        };
        let mut m = meta();
        m.advisor_enabled = true;
        let spec = resolve(&settings, &m, &[provider("p1")]).unwrap();
        assert!(spec.model.is_empty());
    }

    #[test]
    fn validate_selection_rules() {
        let providers = vec![provider("p1")];
        assert!(validate_selection(true, None, None, None, &providers).is_ok());
        assert!(validate_selection(
            true,
            Some("p1"),
            Some("m1"),
            Some(EffortLevel::High),
            &providers
        )
        .is_ok());
        let err = validate_selection(true, Some("gone"), None, None, &providers).unwrap_err();
        assert_eq!(err, "That provider is no longer configured");
        let err = validate_selection(true, None, Some("m1"), None, &providers).unwrap_err();
        assert_eq!(err, "Pick a provider before setting a model");
        let err =
            validate_selection(true, None, None, Some(EffortLevel::High), &providers).unwrap_err();
        assert_eq!(err, "Pick a provider before setting a model");
    }

    #[test]
    fn validate_selection_never_blocks_a_disable() {
        let providers = vec![provider("p1")];
        // an enable naming a deleted provider is still refused…
        assert_eq!(
            validate_selection(
                true,
                Some("gone"),
                Some("m1"),
                Some(EffortLevel::High),
                &providers
            )
            .unwrap_err(),
            "That provider is no longer configured"
        );
        // …but a disable is always accepted, whatever the stored pick is
        assert!(validate_selection(false, Some("gone"), None, None, &providers).is_ok());
        assert!(validate_selection(false, None, Some("m1"), None, &providers).is_ok());
        assert!(validate_selection(
            false,
            Some("gone"),
            Some("m1"),
            Some(EffortLevel::High),
            &providers
        )
        .is_ok());
    }

    #[test]
    fn selection_validation_treats_blank_as_unset() {
        let providers = vec![provider("p1")];
        // a blank provider id clears the override: nothing else is set → Ok
        assert!(validate_selection(true, Some(""), None, None, &providers).is_ok());
        // but a model without a provider is still rejected
        assert_eq!(
            validate_selection(true, Some(""), Some("m1"), None, &providers).unwrap_err(),
            "Pick a provider before setting a model"
        );
        assert_eq!(
            validate_selection(true, None, Some("m1"), None, &providers).unwrap_err(),
            "Pick a provider before setting a model"
        );
    }

    fn user(text: &str) -> Msg {
        Msg::User {
            text: text.into(),
            ts: None,
        }
    }

    fn assistant(text: &str, calls: Vec<(&str, &str)>) -> Msg {
        Msg::Assistant {
            text: text.into(),
            tool_calls: calls
                .into_iter()
                .map(|(id, name)| ToolCall {
                    id: id.into(),
                    name: name.into(),
                    arguments: serde_json::json!({}),
                })
                .collect(),
            ts: None,
        }
    }

    fn tool_result(id: &str, text: &str) -> Msg {
        Msg::ToolResult {
            call_id: id.into(),
            text: text.into(),
            is_error: false,
        }
    }

    fn system_text(messages: &[Msg]) -> String {
        messages
            .iter()
            .find_map(|m| match m {
                Msg::System { text } => Some(text.clone()),
                _ => None,
            })
            .expect("advisor request has a system message")
    }

    fn names() -> Vec<String> {
        vec!["ducky__fs_read".into(), "ducky__advisor".into()]
    }

    #[test]
    fn build_messages_leads_with_one_system_message() {
        let history = vec![user("hello")];
        let msgs = build_messages(&history, &names(), "You are the executor.");
        assert_eq!(
            msgs.iter()
                .filter(|m| matches!(m, Msg::System { .. }))
                .count(),
            1
        );
        assert!(matches!(msgs[0], Msg::System { .. }));
        let system = system_text(&msgs);
        assert!(system.contains("advisor-strategy pattern"));
        assert!(system.contains("You are the executor."));
        assert!(system.contains("ducky__advisor"));
        assert!(system.contains("<executor-system-prompt>"));
        assert!(system.contains("</executor-system-prompt>"));
        let prompt_start = system.find("<executor-system-prompt>").unwrap();
        let prompt_end = system.find("</executor-system-prompt>").unwrap();
        assert!(prompt_start < prompt_end);
        assert!(
            system[prompt_start + "<executor-system-prompt>".len()..prompt_end]
                .contains("You are the executor.")
        );
    }

    #[test]
    fn build_messages_strips_the_in_flight_tool_calls_and_keeps_text() {
        let history = vec![
            user("do the thing"),
            assistant("I should ask the advisor", vec![("c1", "ducky__advisor")]),
        ];
        let msgs = build_messages(&history, &names(), "sys");
        let Msg::Assistant {
            text, tool_calls, ..
        } = &msgs[2]
        else {
            panic!("assistant message survives with its text");
        };
        assert_eq!(text, "I should ask the advisor");
        assert!(tool_calls.is_empty());
        assert!(matches!(msgs.last(), Some(Msg::User { .. })));
    }

    #[test]
    fn build_messages_drops_an_empty_trailing_assistant_message() {
        let history = vec![
            user("do the thing"),
            tool_result("c0", "some result"),
            assistant("", vec![("c1", "ducky__advisor")]),
        ];
        let msgs = build_messages(&history, &names(), "sys");
        // system + user + tool_result + the synthetic ask
        assert_eq!(msgs.len(), 4);
        assert!(matches!(msgs[2], Msg::ToolResult { .. }));
        assert!(matches!(msgs[3], Msg::User { .. }));
    }

    #[test]
    fn build_messages_keeps_a_user_tail_and_appends_the_ask_once() {
        let history = vec![user("do the thing")];
        let msgs = build_messages(&history, &names(), "sys");
        assert_eq!(msgs.len(), 3);
        let Msg::User { text, .. } = &msgs[2] else {
            panic!("ask is a user message")
        };
        assert_eq!(text, ADVISOR_ASK);
    }

    #[test]
    fn build_messages_strips_only_the_unanswered_calls_of_a_batch() {
        // a batch ran in order: `fs_read` has its result in the history while
        // the in-flight advisor call does not — providers reject that shape
        let history = vec![
            user("do the thing"),
            assistant(
                "let me look",
                vec![("c1", "ducky__fs_read"), ("c2", "ducky__advisor")],
            ),
            tool_result("c1", "file contents"),
        ];
        let msgs = build_messages(&history, &names(), "sys");
        let Msg::Assistant {
            text, tool_calls, ..
        } = &msgs[2]
        else {
            panic!("assistant message survives with its text");
        };
        assert_eq!(text, "let me look");
        assert_eq!(tool_calls.len(), 1);
        assert_eq!(tool_calls[0].id, "c1");
        assert!(matches!(msgs.last(), Some(Msg::User { text, .. }) if text == ADVISOR_ASK));

        // the orderings that already worked: in-flight as the only call,
        // with text and with none
        for history in [
            vec![
                user("do the thing"),
                assistant("I should ask the advisor", vec![("c1", "ducky__advisor")]),
            ],
            vec![
                user("do the thing"),
                tool_result("c0", "some result"),
                assistant("", vec![("c1", "ducky__advisor")]),
            ],
        ] {
            let msgs = build_messages(&history, &names(), "sys");
            assert!(!msgs
                .iter()
                .any(|m| matches!(m, Msg::Assistant { tool_calls, .. } if !tool_calls.is_empty())));
            assert!(matches!(msgs.last(), Some(Msg::User { text, .. }) if text == ADVISOR_ASK));
        }
    }

    #[test]
    fn build_messages_keeps_earlier_advisor_guidance() {
        let history = vec![
            user("do the thing"),
            assistant("", vec![("c1", "ducky__advisor")]),
            tool_result("c1", "Split the migration in two commits."),
            assistant("working", vec![("c2", "ducky__advisor")]),
        ];
        let msgs = build_messages(&history, &names(), "sys");
        assert!(msgs.iter().any(|m| matches!(
            m,
            Msg::ToolResult { text, .. } if text == "Split the migration in two commits."
        )));
    }

    #[test]
    fn build_messages_reports_no_tools() {
        let msgs = build_messages(&[user("hi")], &[], "sys");
        assert!(system_text(&msgs).contains("Tools the executor can call: none"));
    }

    #[test]
    fn error_texts_are_pinned() {
        assert_eq!(
            err_no_model(),
            "No advisor model is configured. Pick one in the chat header."
        );
        assert_eq!(err_failed("boom"), "Advisor call failed: boom");
        assert!(err_cancelled().contains("cancelled"));
        assert_eq!(err_empty(), "Advisor returned no text content.");
    }
}
