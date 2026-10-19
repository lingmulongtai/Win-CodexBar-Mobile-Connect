import type { DesktopPeerStatus } from "../types/bridge";
import { useLocale } from "../hooks/useLocale";
import "./DesktopPeerBanner.css";

export function DesktopPeerBanner({ status }: { status: DesktopPeerStatus | null | undefined }) {
  const { t } = useLocale();
  if (!status?.selectedId) return null;
  return <div className="desktop-peer-banner" role="status">
    <strong>{t("DesktopPeerSource")}: {status.name}</strong>
    <p>{t(status.receiving ? "DesktopPeerReceiving" : status.connected ? "DesktopPeerConnected" : "DesktopPeerOffline")}
      {status.lastReceived != null && ` · ${new Date(status.lastReceived * 1000).toLocaleString()}`}</p>
    {status.error && <p>{status.error}</p>}
  </div>;
}
