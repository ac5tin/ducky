//! The advisor: a conversation-scoped second-opinion model the executor can
//! consult. Selection resolution, request assembly and the pinned copy live
//! here; the provider call and the tool card live in `agent.rs`.

use crate::config::{AppSettings, ConversationMeta, EffortLevel, ProviderConfig};

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

pub fn validate_selection(
    provider_id: Option<&str>,
    model: Option<&str>,
    effort: Option<EffortLevel>,
    providers: &[ProviderConfig],
) -> Result<(), String> {
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
        assert!(validate_selection(None, None, None, &providers).is_ok());
        assert!(
            validate_selection(Some("p1"), Some("m1"), Some(EffortLevel::High), &providers).is_ok()
        );
        let err = validate_selection(Some("gone"), None, None, &providers).unwrap_err();
        assert_eq!(err, "That provider is no longer configured");
        let err = validate_selection(None, Some("m1"), None, &providers).unwrap_err();
        assert_eq!(err, "Pick a provider before setting a model");
        let err = validate_selection(None, None, Some(EffortLevel::High), &providers).unwrap_err();
        assert_eq!(err, "Pick a provider before setting a model");
    }
}
