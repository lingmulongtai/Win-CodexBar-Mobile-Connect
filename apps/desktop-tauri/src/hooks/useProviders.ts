import { useCallback, useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import type {
  ProviderUsageSnapshot,
  DesktopPeerStatus,
  RefreshCompletePayload,
  RefreshStartedPayload,
} from "../types/bridge";
import {
  getCachedProviders,
  getDesktopPeerStatus,
  refreshProviders,
  refreshProvidersIfStale,
} from "../lib/tauri";

/**
 * Longest delay `setTimeout` can hold: browsers, WebView2 included, store it
 * as a signed 32-bit integer. A longer delay wraps around and fires early:
 * a reset 24.9 to 49.7 days away would refresh at once.
 */
const MAX_TIMER_DELAY_MS = 2_147_483_647;

export interface UseProvidersOptions {
  /**
   * Delay the automatic stale-aware refresh on mount. Tray/menu surfaces use
   * this so opening the UI can paint and accept input before provider work
   * starts.
   */
  initialRefreshDelayMs?: number;
  /**
   * Whether mounting this hook should ask the backend for a stale-aware refresh.
   * Passive surfaces can turn this off when another timer already drives
   * freshness, while still receiving cached data and live provider events.
   */
  refreshOnMount?: boolean;
  /**
   * When true, the mount refresh bypasses stale-cache checks and refreshes all
   * enabled providers. Used by the tray/menu "refresh on open" setting.
   */
  forceRefreshOnMount?: boolean;
}

export interface UseProvidersResult {
  peer: DesktopPeerStatus | null;
  /** Current provider snapshots (updated live as each provider completes). */
  providers: ProviderUsageSnapshot[];
  /** True while a refresh cycle is in progress. */
  isRefreshing: boolean;
  refreshingProviderIds: ReadonlySet<string>;
  /** Trigger a manual refresh. No-op if already refreshing. */
  refresh: () => void;
  /** Summary from the last completed refresh cycle, if any. */
  lastRefresh: RefreshCompletePayload | null;
  /** True when the hook has provider data that can stay visible during refresh. */
  hasCachedData: boolean;
  /** True after the initial cached-provider read has completed. */
  hasLoadedCache: boolean;
}

/**
 * Subscribe to live provider usage data.
 *
 * On mount the hook:
 *  1. Loads any cached providers already in AppState.
 *  2. Fires `refresh_providers` to kick off a fresh fetch cycle.
 *  3. Listens for `provider-updated` events and merges each snapshot
 *     into the local array (upsert by providerId).
 *  4. Listens for `refresh-started` / `refresh-complete` to track loading.
 */
export function useProviders(options: UseProvidersOptions = {}): UseProvidersResult {
  const [providers, setProviders] = useState<ProviderUsageSnapshot[]>([]);
  const [refreshingProviderIds, setRefreshingProviderIds] = useState<Set<string>>(
    new Set(),
  );
  const [lastRefresh, setLastRefresh] = useState<RefreshCompletePayload | null>(
    null,
  );
  const [hasLoadedCache, setHasLoadedCache] = useState(false);
  const [peer, setPeer] = useState<DesktopPeerStatus | null>(null);
  const sourceRef = useRef<string>("loading");
  const refreshingRef = useRef(false);
  const pendingSnapshotsRef = useRef<Map<string, ProviderUsageSnapshot>>(new Map());
  const flushTimerRef = useRef<number | undefined>(undefined);
  const resetRefreshTimerRef = useRef<number | undefined>(undefined);
  const settingsReloadEpochRef = useRef(0);
  const settingsReloadingRef = useRef(false);

  const mergeSnapshots = useCallback((snapshots: ProviderUsageSnapshot[]) => {
    if (snapshots.length === 0) return;
    setProviders((prev) => {
      const next = [...prev];
      const byId = new Map(next.map((provider, index) => [provider.providerId, index]));
      for (const snapshot of snapshots) {
        const idx = byId.get(snapshot.providerId);
        if (idx !== undefined) {
          next[idx] = snapshot;
        } else {
          byId.set(snapshot.providerId, next.length);
          next.push(snapshot);
        }
      }
      return next;
    });
  }, []);

  const flushPendingSnapshots = useCallback(() => {
    if (flushTimerRef.current !== undefined) {
      window.clearTimeout(flushTimerRef.current);
      flushTimerRef.current = undefined;
    }
    const snapshots = Array.from(pendingSnapshotsRef.current.values());
    pendingSnapshotsRef.current.clear();
    mergeSnapshots(snapshots);
  }, [mergeSnapshots]);

  const queueSnapshot = useCallback((snapshot: ProviderUsageSnapshot) => {
    pendingSnapshotsRef.current.set(snapshot.providerId, snapshot);
    if (settingsReloadingRef.current || flushTimerRef.current !== undefined) return;
    flushTimerRef.current = window.setTimeout(flushPendingSnapshots, 80);
  }, [flushPendingSnapshots]);

  const refresh = useCallback(() => {
    if (refreshingRef.current) return;
    refreshingRef.current = true;
    refreshProviders().catch(() => {
      refreshingRef.current = false;
      setRefreshingProviderIds(new Set());
    }).finally(() => {
      if (sourceRef.current.startsWith("peer:")) refreshingRef.current = false;
    });
  }, []);

  useEffect(() => {
    let cancelled = false;
    let sourceLoaded = false;

    const receiveSource = (status: DesktopPeerStatus) => {
      if (cancelled) return;
      const changed = sourceRef.current !== status.sourceId;
      sourceRef.current = status.sourceId;
      setPeer(status);
      if (changed) {
        setProviders([]);
        setLastRefresh(null);
        refreshingRef.current = false;
        setRefreshingProviderIds(new Set());
      }
      if (sourceLoaded && !changed && status.sourceId === "local") return;
      sourceLoaded = true;
      const epoch = ++settingsReloadEpochRef.current;
      settingsReloadingRef.current = status.sourceId === "local";
      pendingSnapshotsRef.current.clear();
      if (flushTimerRef.current !== undefined) {
        window.clearTimeout(flushTimerRef.current);
        flushTimerRef.current = undefined;
      }
      void getCachedProviders().then((cached) => {
        if (cancelled || epoch !== settingsReloadEpochRef.current) return;
        const matches = cached.every((p) => status.sourceId === "local"
          ? !p.sourceLabel.startsWith("peer:")
          : p.sourceLabel.startsWith(`${status.sourceId}|`));
        if (matches) {
          if (status.sourceId === "local") mergeSnapshots(cached);
          else setProviders(cached.map(p => ({ ...p, peerReceivedAt: status.lastReceived })));
        }
      }).catch(() => {}).finally(() => {
        if (!cancelled && epoch === settingsReloadEpochRef.current) {
          setHasLoadedCache(true);
          settingsReloadingRef.current = false;
          if (status.sourceId === "local") flushPendingSnapshots();
        }
      });
    };
    const sourceEpoch = settingsReloadEpochRef.current;
    void getDesktopPeerStatus().then((status) => {
      if (sourceEpoch === settingsReloadEpochRef.current) receiveSource(status);
    }).catch(() => { if (!cancelled) setHasLoadedCache(true); });
    const unlistenPeer = listen<DesktopPeerStatus>("desktop-peer-changed", event => receiveSource(event.payload));

    // Event listeners.
    const unlistenUpdated = listen<ProviderUsageSnapshot>(
      "provider-updated",
      (event) => {
        if (!cancelled && sourceRef.current === "local") {
          queueSnapshot(event.payload);
          setRefreshingProviderIds(
            (current) =>
              new Set(
                [...current].filter((id) => id !== event.payload.providerId),
              ),
          );
        }
      },
    );

    const unlistenSettings = listen("settings-changed", () => {
      if (sourceRef.current !== "local") {
        void getDesktopPeerStatus().then(receiveSource).catch(() => {});
        return;
      }
      const epoch = ++settingsReloadEpochRef.current;
      settingsReloadingRef.current = true;
      if (flushTimerRef.current !== undefined) {
        window.clearTimeout(flushTimerRef.current);
        flushTimerRef.current = undefined;
      }
      pendingSnapshotsRef.current.clear();
      getCachedProviders()
        .then((cached) => {
          if (!cancelled && epoch === settingsReloadEpochRef.current) {
            mergeSnapshots(cached);
          }
        })
        .finally(() => {
          if (!cancelled && epoch === settingsReloadEpochRef.current) {
            settingsReloadingRef.current = false;
            flushPendingSnapshots();
          }
        });
    });

    const unlistenStarted = listen<RefreshStartedPayload>("refresh-started", (event) => {
      if (!cancelled && sourceRef.current === "local") {
        refreshingRef.current = true;
        setRefreshingProviderIds(new Set(event.payload.providerIds));
      }
    });

    const unlistenComplete = listen<RefreshCompletePayload>(
      "refresh-complete",
      (event) => {
        if (!cancelled && sourceRef.current === "local") {
          if (!settingsReloadingRef.current) flushPendingSnapshots();
          refreshingRef.current = false;
          setRefreshingProviderIds(new Set());
          setLastRefresh(event.payload);
        }
      },
    );

    let initialRefreshTimer: number | undefined;

    const runInitialRefresh = () => {
      const refreshPromise = options.forceRefreshOnMount
        ? refreshProviders()
        : refreshProvidersIfStale();
      refreshPromise.catch(() => {
        if (!cancelled) {
          refreshingRef.current = false;
          setRefreshingProviderIds(new Set());
        }
      });
    };

    // Kick off the initial refresh, but let the backend reuse fresh cache.
    if (options.refreshOnMount !== false) {
      const delay = Math.max(0, options.initialRefreshDelayMs ?? 0);
      if (delay > 0) {
        initialRefreshTimer = window.setTimeout(runInitialRefresh, delay);
      } else {
        runInitialRefresh();
      }
    }

    return () => {
      cancelled = true;
      settingsReloadEpochRef.current += 1;
      settingsReloadingRef.current = false;
      if (initialRefreshTimer !== undefined) {
        window.clearTimeout(initialRefreshTimer);
      }
      if (flushTimerRef.current !== undefined) {
        window.clearTimeout(flushTimerRef.current);
        flushTimerRef.current = undefined;
      }
      if (resetRefreshTimerRef.current !== undefined) {
        window.clearTimeout(resetRefreshTimerRef.current);
        resetRefreshTimerRef.current = undefined;
      }
      pendingSnapshotsRef.current.clear();
      unlistenUpdated.then((fn) => fn());
      unlistenPeer.then((fn) => fn());
      unlistenSettings.then((fn) => fn());
      unlistenStarted.then((fn) => fn());
      unlistenComplete.then((fn) => fn());
    };
  }, [
    options.forceRefreshOnMount,
    options.initialRefreshDelayMs,
    options.refreshOnMount,
    flushPendingSnapshots,
    mergeSnapshots,
    queueSnapshot,
  ]);

  useEffect(() => {
    if (resetRefreshTimerRef.current !== undefined) {
      window.clearTimeout(resetRefreshTimerRef.current);
      resetRefreshTimerRef.current = undefined;
    }

    const now = Date.now();
    let nextReset: number | undefined;
    for (const provider of providers) {
      const candidates = [
        provider.primary.resetsAt,
        provider.secondary?.resetsAt,
        provider.modelSpecific?.resetsAt,
        provider.tertiary?.resetsAt,
        ...(provider.extraRateWindows ?? []).map((extra) => extra.window.resetsAt),
        provider.cost?.resetsAt,
      ];
      for (const value of candidates) {
        if (!value) continue;
        const time = Date.parse(value);
        if (Number.isFinite(time) && time > now && (nextReset === undefined || time < nextReset)) {
          nextReset = time;
        }
      }
    }

    if (nextReset === undefined) return;

    const resetAt = nextReset;
    const armResetRefresh = () => {
      const remaining = resetAt - Date.now() + 1_000;
      if (remaining > MAX_TIMER_DELAY_MS) {
        // Too far away for one timer: wait the longest delay, then re-check.
        resetRefreshTimerRef.current = window.setTimeout(armResetRefresh, MAX_TIMER_DELAY_MS);
        return;
      }
      resetRefreshTimerRef.current = window.setTimeout(() => {
        resetRefreshTimerRef.current = undefined;
        refresh();
      }, Math.max(5_000, remaining));
    };
    armResetRefresh();

    return () => {
      if (resetRefreshTimerRef.current !== undefined) {
        window.clearTimeout(resetRefreshTimerRef.current);
        resetRefreshTimerRef.current = undefined;
      }
    };
  }, [providers, refresh]);

  return {
    peer,
    providers,
    isRefreshing: peer?.selectedId ? peer.receiving : refreshingProviderIds.size > 0,
    refreshingProviderIds: peer?.selectedId
      ? new Set(peer.receiving ? providers.map(p => p.providerId) : []) : refreshingProviderIds,
    refresh,
    lastRefresh,
    hasCachedData: providers.length > 0,
    hasLoadedCache,
  };
}
