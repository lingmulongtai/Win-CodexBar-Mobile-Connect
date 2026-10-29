import { useEffect, useState } from "react";
import { Field, Select, SettingsGroup, Toggle, useFieldAria } from "../../../components/FormControls";
import { useLocale } from "../../../hooks/useLocale";
import { getDiscordPresenceStatus, openExternalUrl, resolveCodexPet } from "../../../lib/tauri";
import type { DiscordPresenceOptions, DiscordPresenceStatus } from "../../../types/bridge";
import type { LocaleKey } from "../../../i18n/keys";
import type { TabProps } from "../settingsTabs";

const defaults: DiscordPresenceOptions = {
  priority: "recent", format: "detailed", showModel: true, showMode: true,
  showElapsed: true, showSessionTokens: true, showToday: true, showWeek: true,
  showMonth: true, showGithub: true, githubUrl: "", showEmoji: true, imageUrl: "",
};
const fields: [keyof DiscordPresenceOptions, LocaleKey][] = [
  ["showModel", "DiscordShowModel"], ["showMode", "DiscordShowMode"],
  ["showElapsed", "DiscordShowElapsed"], ["showSessionTokens", "DiscordShowSession"],
  ["showToday", "DiscordShowToday"], ["showWeek", "DiscordShowWeek"],
  ["showMonth", "DiscordShowMonth"], ["showGithub", "DiscordShowGithub"],
  ["showEmoji", "DiscordShowEmoji"],
];

function ImageUrl({ value, onChange, disabled }: { value: string; onChange: (v: string) => void; disabled: boolean }) {
  const aria = useFieldAria();
  const { t } = useLocale();
  const [draft, setDraft] = useState(value);
  const [editing, setEditing] = useState(false);
  const [invalid, setInvalid] = useState(false);
  const [busy, setBusy] = useState(false);
  useEffect(() => { if (!editing && !busy) { setDraft(value); setInvalid(false); } }, [value, editing, busy]);
  const commit = async () => {
    if (busy) return;
    let valid = !draft;
    let petLink = false;
    try {
      const url = new URL(draft);
      valid = url.protocol === "https:" && !url.username && !url.password && draft.length <= 512;
      petLink = url.hostname === "codex-pets.net" && (url.pathname.startsWith("/share/") || url.hash.startsWith("#/pets/"));
      if (!petLink && url.hash) valid = false;
    } catch { /* Empty is allowed and resets the image. */ }
    if (!valid) { setInvalid(true); return; }
    if (petLink) {
      setBusy(true);
      try { onChange(await resolveCodexPet(draft)); setEditing(false); }
      catch { setInvalid(true); }
      finally { setBusy(false); }
    } else {
      if (draft !== value) onChange(draft);
      setEditing(false);
    }
  };
  return <div><input className="discord-image-url" type="url" {...aria} value={draft} disabled={disabled || busy}
    aria-invalid={invalid} placeholder="https://…" onFocus={() => setEditing(true)}
    onChange={(e) => { setDraft(e.target.value); setInvalid(false); }} onBlur={() => void commit()}
    onKeyDown={(e) => { if (e.key === "Enter") e.currentTarget.blur(); }} />
    {busy && <div role="status">{t("DiscordPetLoading")}</div>}
    {invalid && <div role="alert">{t("DiscordPetInvalid")}</div>}
  </div>;
}

const defaultGithubUrl = "https://github.com/lingmulongtai/Win-CodexBar-Mobile-Connect";

function isGithubUrl(value: string): boolean {
  try {
    const url = new URL(value);
    return url.protocol === "https:" && url.hostname === "github.com" && (!url.port || url.port === "443") && !url.username && !url.password && value.length <= 512;
  } catch { return false; }
}

function GithubUrl({ value, onChange, disabled }: { value: string; onChange: (v: string) => void; disabled: boolean }) {
  const aria = useFieldAria();
  const { t } = useLocale();
  const [draft, setDraft] = useState(value);
  const [editing, setEditing] = useState(false);
  const [invalid, setInvalid] = useState(false);
  useEffect(() => { if (!editing) { setDraft(value); setInvalid(false); } }, [value, editing]);
  const commit = () => {
    const valid = !draft || isGithubUrl(draft);
    setInvalid(!valid);
    if (valid) { if (draft !== value) onChange(draft); setEditing(false); }
  };
  return <div><input className="discord-image-url" type="url" {...aria} value={draft} disabled={disabled}
    aria-invalid={invalid} placeholder={defaultGithubUrl} onFocus={() => setEditing(true)}
    onChange={(e) => { setDraft(e.target.value); setInvalid(false); }} onBlur={commit}
    onKeyDown={(e) => { if (e.key === "Enter") e.currentTarget.blur(); }} />
    {invalid && <div role="alert">{t("DiscordGithubInvalid")}</div>}
  </div>;
}

function elapsed(start: number, now: number): string {
  const seconds = Math.max(0, Math.floor(now / 1000 - start));
  const hours = Math.floor(seconds / 3600);
  const minutes = Math.floor(seconds / 60) % 60;
  return hours ? `${hours}:${String(minutes).padStart(2, "0")}:${String(seconds % 60).padStart(2, "0")}`
    : `${minutes}:${String(seconds % 60).padStart(2, "0")}`;
}

export default function DiscordTab({ settings, set, saving }: TabProps) {
  const { t } = useLocale();
  const [snapshot, setSnapshot] = useState<DiscordPresenceStatus | null>(null);
  const [failed, setFailed] = useState(false);
  const [now, setNow] = useState(Date.now());
  const [linkError, setLinkError] = useState(false);
  const options = { ...defaults, ...settings.discordPresenceOptions };
  const change = (patch: Partial<DiscordPresenceOptions>) => set({ discordPresenceOptions: patch });
  useEffect(() => {
    let cancelled = false;
    let pending = false;
    const update = async () => {
      setNow(Date.now());
      if (pending) return;
      pending = true;
      try {
        const value = await getDiscordPresenceStatus();
        if (!cancelled) { setSnapshot(value); setFailed(false); }
      } catch {
        if (!cancelled) { setSnapshot(null); setFailed(true); }
      } finally { pending = false; }
    };
    void update();
    const timer = window.setInterval(() => void update(), 1000);
    return () => { cancelled = true; window.clearInterval(timer); };
  }, []);
  const activity = snapshot?.status === "connected" ? snapshot.activity : null;
  const usageLabels: Record<DiscordPresenceStatus["usage"], LocaleKey> = {
    available: "DiscordUsageAvailable", localOnly: "DiscordUsageLocalOnly", partial: "DiscordUsagePartial", partialLoading: "DiscordUsagePartialLoading",
    loading: "DiscordUsageLoading", unavailable: "DiscordUsageUnavailable", trackingOff: "DiscordUsageTrackingOff", hidden: "DiscordUsageHidden",
  };
  return <>
    <SettingsGroup title={t("DiscordPresenceLabel")}>
      <Field label={t("DiscordPresenceLabel")} description={t("DiscordPresenceHelper")}>
        <Toggle variant="switch" checked={settings.discordPresenceEnabled ?? false} disabled={saving} onChange={(value) => set({ discordPresenceEnabled: value })} />
      </Field>
    </SettingsGroup>
    <SettingsGroup title={t("DiscordPreview")} footer={t("DiscordPreviewHelp")}>
      {activity ? <>
        <div className="discord-preview">
          <div className="discord-preview__body">
            {activity.assets?.large_image && <img src={activity.assets.large_image} alt="" />}
            <div><strong>Win-CodexBar + Mobile Connect</strong>
              {activity.details && <div>{activity.details}</div>}
              {activity.state && <div>{activity.state}</div>}
              {activity.timestamps?.start != null && <div>{elapsed(activity.timestamps.start, now)} elapsed</div>}
            </div>
          </div>
          {activity.buttons?.map((button) => <button key={button.url} type="button" className="discord-preview__button" onClick={() => {
            setLinkError(false);
            if (isGithubUrl(button.url)) void openExternalUrl(button.url).catch(() => setLinkError(true));
          }}>{button.label}</button>)}
          {linkError && <div role="alert">{t("DiscordGithubOpenError")}</div>}
        </div>
        <p role="status">{t(usageLabels[snapshot!.usage])}</p>
      </> : <p role="status">{t(failed ? "DiscordLiveError" : snapshot?.status === "disabled" ? "DiscordLiveDisabled" : "DiscordLiveConnecting")}</p>}
    </SettingsGroup>
    <SettingsGroup title={t("DiscordPriority")}>
      <Field label={t("DiscordPriority")}><Select value={options.priority} disabled={saving} onChange={(value) => change({ priority: value as DiscordPresenceOptions["priority"] })} options={[
        { value: "recent", label: t("DiscordPriorityRecent") }, { value: "codex", label: t("DiscordPriorityCodex") }, { value: "claude", label: t("DiscordPriorityClaude") },
      ]} /></Field>
      <Field label={t("DiscordFormat")}><Select value={options.format} disabled={saving} onChange={(value) => change({ format: value as DiscordPresenceOptions["format"] })} options={[
        { value: "detailed", label: t("DiscordFormatDetailed") }, { value: "compact", label: t("DiscordFormatCompact") },
        { value: "modelFirst", label: t("DiscordFormatModelFirst") }, { value: "service", label: t("DiscordFormatService") },
      ]} /></Field>
    </SettingsGroup>
    <SettingsGroup title={t("DiscordFields")}>{fields.map(([key, label]) => <Field key={key} label={t(label)}>
      <Toggle variant="switch" checked={Boolean(options[key])} disabled={saving} onChange={(value) => change({ [key]: value })} />
    </Field>)}</SettingsGroup>
    <SettingsGroup title={t("DiscordGithubSettings")} footer={t("DiscordGithubHelp")}>
      <Field label={t("DiscordGithubUrl")}><GithubUrl value={options.githubUrl} onChange={(githubUrl) => change({ githubUrl })} disabled={saving} /></Field>
      <Field label={t("DiscordGithubTest")}><button type="button" onClick={() => {
        setLinkError(false);
        void openExternalUrl(isGithubUrl(options.githubUrl) ? options.githubUrl : defaultGithubUrl).catch(() => setLinkError(true));
      }}>{t("DiscordGithubTest")}</button></Field>
      {linkError && <div role="alert">{t("DiscordGithubOpenError")}</div>}
    </SettingsGroup>
    <SettingsGroup title={t("DiscordPet")} footer={t("DiscordPetHelp")}>
      <Field label={t("DiscordPetUrl")}><ImageUrl value={options.imageUrl} onChange={(imageUrl) => change({ imageUrl })} disabled={saving} /></Field>
    </SettingsGroup>
  </>;
}
