import { useState } from "react";
import { Field, SettingsGroup } from "../../../components/FormControls";
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
  return <SettingsGroup title={t("DesktopPeerTitle")} footer={<p>{t("DesktopPeerHelp")}</p>}>
    <Field label={t("DesktopPeerSource")}>
      <select aria-label={t("DesktopPeerSource")} className="select-input" value={status?.selectedId ?? ""}
        disabled={busy || !status || !!status.storageError}
        onChange={event => void run(() => selectDesktopPeer(event.target.value || null))}>
        <option value="">{t("DesktopPeerLocal")}</option>
        {status?.connections.map(pc => <option key={pc.id} value={pc.id}>{pc.name} · {pc.address}</option>)}
      </select>
    </Field>
    <DesktopPeerBanner status={status} />
    <p className="settings-group__helper">{t("DesktopPeerDataHelp")}</p>
    {status?.connections.map(pc => <div key={pc.id} className="settings-group__row">
      <span>{pc.name} · {pc.address}</span>
      <button className="credential-btn credential-btn--danger" disabled={busy || !!status.storageError}
        aria-label={`${t("DesktopPeerRemove")}: ${pc.name}`} onClick={() => void run(() => removeDesktopPeer(pc.id))}>{t("DesktopPeerRemove")}</button>
    </div>)}
    {status?.connections.length === 0 && <p>{t("DesktopPeerEmpty")}</p>}
    <Field label={t("DesktopPeerName")}>
      <input aria-label={t("DesktopPeerName")} className="text-input" value={name} maxLength={64} autoComplete="off"
        disabled={busy || !status || !!status.storageError} onChange={event => setName(event.target.value)} />
    </Field>
    <Field label={t("DesktopPeerCode")}>
      <input aria-label={t("DesktopPeerCode")} type="password" className="text-input" value={code} maxLength={1024}
        autoComplete="off" spellCheck={false} disabled={busy || !status || !!status.storageError}
        onChange={event => setCode(event.target.value)} />
    </Field>
    <button className="credential-btn credential-btn--primary" disabled={busy || !status || !!status.storageError || !name.trim() || !code.trim()}
      onClick={() => void run(async () => { await saveDesktopPeer(name, code); setCode(""); setName(""); })}>{t("DesktopPeerConnect")}</button>
    {(error || status?.storageError) && <p role="alert">{error ?? status?.storageError}</p>}
  </SettingsGroup>;
}
