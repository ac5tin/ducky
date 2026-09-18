import type {
  AppConfig,
  ConversationMeta,
  EffortLevel,
  ProviderConfig,
} from "./types";

/**
 * The model a lazily-created chat starts with: a pick staged on the draft
 * page wins over the app default model, then the provider's own default,
 * then its first fetched model.
 */
export function resolveDraftModel(
  staged: string | null,
  config: AppConfig | null,
  provider: ProviderConfig | null,
): string {
  if (!provider) return "";
  const appDefault =
    config?.settings.default_provider_id === provider.id
      ? config.settings.default_model
      : null;
  return (
    staged || appDefault || provider.default_model || provider.models[0] || ""
  );
}

/**
 * The model the picker shows as current: the conversation's own model, else
 * on the draft page what creation would start the chat with.
 */
export function resolveShownModel(
  conversation: ConversationMeta | undefined,
  activeId: string | null,
  staged: string | null,
  config: AppConfig | null,
  provider: ProviderConfig | null,
): string {
  return (
    conversation?.model ||
    resolveDraftModel(activeId ? null : staged, config, provider)
  );
}

/**
 * The effort the picker shows as current: the conversation's own, else on the
 * draft page the staged pick — an untouched draft previews the app default
 * that creation will seed.
 */
export function resolveShownEffort(
  activeId: string | null,
  conversation: ConversationMeta | undefined,
  staged: EffortLevel | null | undefined,
  config: AppConfig | null,
): EffortLevel | null {
  if (activeId) return conversation?.effort ?? null;
  return staged !== undefined
    ? staged
    : (config?.settings.default_effort ?? null);
}
