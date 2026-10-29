import { Field, SettingsGroup, Toggle } from "../../../components/FormControls";
import { useLocale } from "../../../hooks/useLocale";
import type { TabProps } from "../settingsTabs";

export default function DiscordTab({ settings, set, saving }: TabProps) {
  const { t } = useLocale();
  return (
    <SettingsGroup title={t("DiscordPresenceLabel")}>
      <Field label={t("DiscordPresenceLabel")} description={t("DiscordPresenceHelper")}>
        <Toggle
          variant="switch"
          checked={settings.discordPresenceEnabled ?? false}
          disabled={saving}
          onChange={(value) => set({ discordPresenceEnabled: value })}
        />
      </Field>
    </SettingsGroup>
  );
}
