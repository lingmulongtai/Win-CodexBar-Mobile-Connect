import { useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { getDesktopPeerStatus } from "../lib/tauri";
import type { DesktopPeerStatus } from "../types/bridge";

/** Metadata only: secrets and provider credentials never reach the webview. */
export function useDesktopPeerStatus() {
  const [status, setStatus] = useState<DesktopPeerStatus | null>(null);
  useEffect(() => {
    let cancelled = false;
    let epoch = 0;
    const unlisten = listen<DesktopPeerStatus>("desktop-peer-changed", event => {
      epoch++;
      if (!cancelled) setStatus(event.payload);
    });
    void getDesktopPeerStatus().then(next => {
      if (!cancelled && epoch === 0) setStatus(next);
    }).catch(() => {});
    return () => { cancelled = true; void unlisten.then(fn => fn()); };
  }, []);
  return status;
}
