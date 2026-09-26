import type {
  AppSettings,
  ConversationMeta,
  EffortLevel,
  ProviderConfig,
} from "./types";

export interface AdvisorChipState {
  mode: "off" | "on" | "unresolved";
  /**
   * The raw pick to write back, which can name a provider that is no longer
   * configured (`mode: "unresolved"`); null when none is selected.
   */
  provider_id: string | null;
  /** Raw pick; `""` means "the provider's default model". */
  model: string;
  effort: EffortLevel | null;
}

function modeOf(enabled: boolean, resolved: boolean): AdvisorChipState["mode"] {
  if (!enabled) return "off";
  return resolved ? "on" : "unresolved";
}

/**
 * Display-only mirror of the Rust `advisor::resolve`: which advisor this chat
 * would use. The backend stays authoritative on whether the tool is offered.
 *
 * `meta` is null on the draft page, where the Settings defaults apply. A
 * session override is the whole provider/model/effort triple, so a set
 * provider id wins over Settings even when its model is unset.
 */
export function advisorChipState(
  meta: ConversationMeta | null,
  settings: AppSettings,
  providers: ProviderConfig[],
): AdvisorChipState {
  const who = meta && meta.advisor_provider_id !== null ? meta : settings;
  const enabled = meta ? meta.advisor_enabled : settings.advisor_enabled_by_default;
  const providerId = who.advisor_provider_id;
  const resolved = providers.some((p) => p.id === providerId);

  return {
    mode: modeOf(enabled, resolved),
    provider_id: providerId,
    model: who.advisor_model ?? "",
    effort: who.advisor_effort ?? null,
  };
}
