//! Chat-title helpers: clean model output and pick which model writes the title.

use crate::config::{AppSettings, ConversationMeta, EffortLevel};

pub struct TitleModel {
    pub provider_id: String,
    pub model: String,
    pub effort: Option<EffortLevel>,
}

pub fn sanitize_title(raw: &str) -> String {
    let line = raw.lines().next().unwrap_or("").trim();
    let stripped = if line.len() >= 2
        && ((line.starts_with('"') && line.ends_with('"'))
            || (line.starts_with('\'') && line.ends_with('\'')))
    {
        &line[1..line.len() - 1]
    } else {
        line
    };
    stripped
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(60)
        .collect()
}

pub fn fallback_title(user_text: &str) -> String {
    sanitize_title(&user_text.chars().take(48).collect::<String>())
}

pub fn resolve_title_model(settings: &AppSettings, chat: &ConversationMeta) -> TitleModel {
    match settings
        .title_provider_id
        .as_deref()
        .filter(|id| !id.is_empty())
    {
        None => TitleModel {
            provider_id: chat.provider_id.clone(),
            model: chat.model.clone(),
            effort: chat.effort,
        },
        Some(provider_id) => TitleModel {
            provider_id: provider_id.to_string(),
            model: settings.title_model.clone().unwrap_or_default(),
            effort: settings.title_effort,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chat() -> ConversationMeta {
        ConversationMeta {
            id: "c".into(),
            title: String::new(),
            provider_id: "chat-p".into(),
            model: "chat-m".into(),
            effort: Some(EffortLevel::High),
            created_at: "t".into(),
            updated_at: "t".into(),
        }
    }

    #[test]
    fn sanitize_empty_stays_empty() {
        assert_eq!(sanitize_title(""), "");
        assert_eq!(sanitize_title("   \n"), "");
    }

    #[test]
    fn sanitize_first_line_trim_quotes_whitespace_and_cap() {
        assert_eq!(sanitize_title("  \"Hello   world\"  \nmore"), "Hello world");
        assert_eq!(sanitize_title("'Hi'"), "Hi");
        let long = "a".repeat(80);
        assert_eq!(sanitize_title(&long).chars().count(), 60);
    }

    #[test]
    fn resolve_defaults_to_chat_model_and_effort() {
        let m = resolve_title_model(&AppSettings::default(), &chat());
        assert_eq!(m.provider_id, "chat-p");
        assert_eq!(m.model, "chat-m");
        assert_eq!(m.effort, Some(EffortLevel::High));
    }

    #[test]
    fn resolve_custom_provider_does_not_inherit_chat_effort() {
        let settings = AppSettings {
            title_provider_id: Some("title-p".into()),
            title_model: None,
            title_effort: None,
            ..Default::default()
        };
        let m = resolve_title_model(&settings, &chat());
        assert_eq!(m.provider_id, "title-p");
        assert_eq!(m.model, "");
        assert_eq!(m.effort, None);
    }

    #[test]
    fn resolve_custom_model_and_effort() {
        let settings = AppSettings {
            title_provider_id: Some("title-p".into()),
            title_model: Some("title-m".into()),
            title_effort: Some(EffortLevel::Low),
            ..Default::default()
        };
        let m = resolve_title_model(&settings, &chat());
        assert_eq!(m.provider_id, "title-p");
        assert_eq!(m.model, "title-m");
        assert_eq!(m.effort, Some(EffortLevel::Low));
    }
}
