import { useState } from "react";
import { Field, Select, SettingsGroup } from "../../../components/FormControls";
import { DesktopPeerBanner } from "../../../components/DesktopPeerBanner";
import { useDesktopPeerStatus } from "../../../hooks/useDesktopPeerStatus";
import { useLocale } from "../../../hooks/useLocale";
import { removeDesktopPeer, saveDesktopPeer, selectDesktopPeer } from "../../../lib/tauri";

export default function DesktopPeerSettings() {
  const { t } = useLocale();
  const status = useDesktopPeerStatus();
  const [name, setName] = useState("");
  const [code, setCode] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  async function run(action: () => Promise<unknown>) {
    if (busy) return;
    setBusy(true); setError(null);
    try { await action(); } catch (error) { setError(String(error)); }
    finally { setBusy(false); }
  }
  return <SettingsGroup title={t("DesktopPeerTitle")} footer={<><p>{t("DesktopPeerHelp")}</p>{status?.selectedId && <p>{t("DesktopPeerDataHelp")}</p>}</>}>
    <Field label={t("DesktopPeerSource")}>
      <Select ariaLabel={t("DesktopPeerSource")} variant="menu" value={status?.selectedId ?? ""}
        disabled={busy || !status || !!status.storageError}
        options={[{ value: "", label: t("DesktopPeerLocal") }, ...(status?.connections ?? []).map(pc => ({ value: pc.id, label: `${pc.name} · ${pc.address}` }))]}
        onChange={id => void run(() => selectDesktopPeer(id || null))} />
    </Field>
    <DesktopPeerBanner status={status} />
    {status?.connections.map(pc => <div key={pc.id} className="settings-group__row settings-group__row--split">
      <span>{pc.name} · {pc.address}</span>
      <button className="credential-btn credential-btn--danger" disabled={busy || !!status.storageError}
        aria-label={`${t("DesktopPeerRemove")}: ${pc.name}`} onClick={() => void run(() => removeDesktopPeer(pc.id))}>{t("DesktopPeerRemove")}</button>
    </div>)}
    {status?.connections.length === 0 && <div className="settings-group__row">{t("DesktopPeerEmpty")}</div>}
    <Field label={t("DesktopPeerName")}>
      <input aria-label={t("DesktopPeerName")} className="text-input" value={name} maxLength={64} autoComplete="off"
        disabled={busy || !status || !!status.storageError} onChange={event => setName(event.target.value)} />
    </Field>
    <Field label={t("DesktopPeerCode")}>
      <input aria-label={t("DesktopPeerCode")} type="password" className="text-input" value={code} maxLength={1024}
        autoComplete="off" spellCheck={false} disabled={busy || !status || !!status.storageError}
        onChange={event => setCode(event.target.value)} />
    </Field>
    <div className="settings-group__row"><button className="credential-btn credential-btn--primary" disabled={busy || !status || !!status.storageError || !name.trim() || !code.trim()}
      onClick={() => void run(async () => { await saveDesktopPeer(name, code); setCode(""); setName(""); })}>{t("DesktopPeerConnect")}</button></div>
    {(error || status?.storageError) && <p className="settings-group__error" role="alert">{error ?? status?.storageError}</p>}
  </SettingsGroup>;
}
