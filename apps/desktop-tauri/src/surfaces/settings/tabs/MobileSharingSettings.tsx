import { useEffect, useState } from "react";
import { Field, SettingsGroup, Toggle } from "../../../components/FormControls";
import { useLocale } from "../../../hooks/useLocale";
import { configureMobileSharing, getMobilePairingCode, getMobileSharingStatus, getMobileNetworkAddresses, revokeMobilePairing, setMobileRemoteSettings } from "../../../lib/tauri";
import type { MobileNetworkAddresses, MobileSharingStatus } from "../../../types/bridge";
import DesktopPeerSettings from "./DesktopPeerSettings";

export default function MobileSharingSettings() {
  const { t } = useLocale();
  const [status, setStatus] = useState<MobileSharingStatus | null>(null);
  const [address, setAddress] = useState("");
  const [port, setPort] = useState("43825");
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState<string | null>(null);
  const [addresses, setAddresses] = useState<MobileNetworkAddresses | null>(null);
  const [discovering, setDiscovering] = useState(true);
  useEffect(() => {
    let cancelled = false;
    getMobileSharingStatus().then((next) => {
      if (cancelled) return;
      setStatus(next); setAddress(next.address); setPort(String(next.port));
    }).catch(() => { if (!cancelled) setMessage(t("MobileSharingFailed")); });
    getMobileNetworkAddresses().then((next) => {
      if (!cancelled) setAddresses(next);
    }).catch(() => { /* Manual numeric addresses remain available. */ })
      .finally(() => { if (!cancelled) setDiscovering(false); });
    const timer = window.setInterval(() => {
      void getMobileSharingStatus().then((next) => { if (!cancelled) setStatus(next); })
        .catch(() => { if (!cancelled) setMessage(t("MobileSharingFailed")); });
    }, 5000);
    return () => { cancelled = true; window.clearInterval(timer); };
  }, [t]);
  async function run(action: () => Promise<void>) {
    if (busy) return;
    setBusy(true); setMessage(null);
    try { await action(); } catch (error) { setMessage(`${t("MobileSharingFailed")} ${String(error)}`); }
    finally { setBusy(false); }
  }
  async function save(enabled: boolean, endpoint = address) {
    const parsedPort = Number(port);
    if (!Number.isInteger(parsedPort) || parsedPort < 1024 || parsedPort > 65535) throw new Error(t("MobileSharingPort"));
    const next = await configureMobileSharing(enabled, endpoint, parsedPort);
    setStatus(next); setAddress(next.address); setPort(String(next.port));
  }
  return <><DesktopPeerSettings /><SettingsGroup title={t("MobileSharingTitle")} footer={<p>{t("MobileSharingHelper")}</p>}>
    <div className="settings-group__row">
      <strong role="status">{status?.running ? t("MobileSharingRunning") : status?.enabled ? t("MobileSharingPending") : t("MobileSharingStopped")}</strong>
      {status?.running && <button className="credential-btn credential-btn--primary" disabled={busy} onClick={() => void run(async () => {
        await navigator.clipboard.writeText(await getMobilePairingCode()); setMessage(t("MobileSharingCopied"));
      })}>{t("MobileSharingCopy")}</button>}
    </div>
    {status?.error && <p role="alert">{status.error}</p>}
    <p className="settings-group__helper">{t("MobileSharingListeningHelp")}</p>
    <Field label={t("MobileSharingEnable")}>
      <Toggle variant="switch" checked={status?.enabled ?? false} ariaLabel={t("MobileSharingEnable")} disabled={busy || !status} onChange={(enabled) => void run(async () => {
        if (!enabled && status) setStatus(await configureMobileSharing(false, status.address, status.port));
        else await save(enabled);
      })} />
    </Field>
    <div className="settings-group__row">
      <button className="credential-btn" disabled={busy || discovering || !status || !addresses?.tailscaleAddress}
        onClick={() => void run(() => save(status?.enabled ?? false, addresses?.tailscaleAddress ?? address))}>
        {t("MobileSharingTailscale")}
      </button>
      <button className="credential-btn" disabled={busy || discovering || !status || !addresses?.lanAddress}
        onClick={() => void run(() => save(status?.enabled ?? false, addresses?.lanAddress ?? address))}>
        {t("MobileSharingLan")}
      </button>
      <button className="credential-btn" disabled={busy || discovering} onClick={() => void run(async () => {
        setDiscovering(true);
        try { setAddresses(await getMobileNetworkAddresses()); } finally { setDiscovering(false); }
      })}>{t("MobileSharingDiscover")}</button>
    </div>
    <p className="settings-group__helper">{t("MobileSharingTailscaleHelper")}</p>
    <Field label={t("MobileSharingRemoteSettings")}>
      <Toggle variant="switch" checked={status?.allowRemoteSettings ?? false} ariaLabel={t("MobileSharingRemoteSettings")} disabled={busy || !status}
        onChange={(allowed) => void run(async () => { setStatus(await setMobileRemoteSettings(allowed)); })} />
    </Field>
    <p className="settings-group__helper">{t("MobileSharingRemoteSettingsHelp")}</p>
    {!discovering && !addresses?.tailscaleAddress && <p>{t("MobileSharingTailscaleUnavailable")}</p>}
    <Field label={t("MobileSharingAddress")}>
      <input className="text-input" aria-label={t("MobileSharingAddress")} value={address} disabled={busy || !status} onChange={(event) => setAddress(event.target.value)} autoComplete="off" spellCheck={false} placeholder="192.168.1.10" />
    </Field>
    <Field label={t("MobileSharingPort")}>
      <input className="text-input" aria-label={t("MobileSharingPort")} type="number" min={1024} max={65535} value={port} disabled={busy || !status} onChange={(event) => setPort(event.target.value)} />
    </Field>
    <div className="settings-group__row">
      <button className="credential-btn" disabled={busy || !status} onClick={() => void run(() => save(status?.enabled ?? false))}>{t("MobileSharingSave")}</button>
      <button className="credential-btn credential-btn--danger" disabled={busy || !status} onClick={() => void run(async () => {
        setStatus(await revokeMobilePairing()); setMessage(t("MobileSharingRevoked"));
      })}>{t("MobileSharingRevoke")}</button>
    </div>
    {message && <p aria-live="polite">{message}</p>}
  </SettingsGroup></>;
}
