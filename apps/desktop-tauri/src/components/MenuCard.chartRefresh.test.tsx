import { render, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

const tauriMocks = vi.hoisted(() => ({
  getProviderChartData: vi.fn(),
  getDeepSeekPricingStatus: vi.fn(),
  getLocaleStrings: vi.fn(),
  setUiLanguage: vi.fn(),
  claudeAccountsList: vi.fn(),
  getCodexAccountsState: vi.fn(),
}));

vi.mock("../lib/tauri", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../lib/tauri")>()),
  ...tauriMocks,
}));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn().mockResolvedValue(() => {}) }));

import { LocaleProvider } from "../i18n/LocaleProvider";
import { buildBundle } from "../test/localeHarness";
import type { ProviderUsageSnapshot } from "../types/bridge";
import MenuCard from "./MenuCard";

const window0 = {
  usedPercent: 10,
  remainingPercent: 90,
  windowMinutes: null,
  resetsAt: null,
  resetDescription: null,
  isExhausted: false,
  reservePercent: null,
  reserveDescription: null,
  reserveWillLastToReset: false,
  reserveEtaSeconds: null,
};

function snapshot(updatedAt: string): ProviderUsageSnapshot {
  return {
    providerId: "claude",
    displayName: "Claude",
    primary: window0,
    selectedMetric: window0,
    secondary: null,
    modelSpecific: null,
    tertiary: null,
    extraRateWindows: [],
    cost: null,
    planName: null,
    accountEmail: null,
    sourceLabel: "oauth",
    updatedAt,
    error: null,
    errorState: "unknown",
    pace: null,
    accountOrganization: null,
    trayStatusLabel: null,
    fetchDurationMs: null,
  };
}

function card(updatedAt: string, override: Partial<ProviderUsageSnapshot> = {}) {
  return (
    <LocaleProvider>
      <MenuCard
        provider={{ ...snapshot(updatedAt), ...override }}
        display={{ hideEmail: false, resetTimeRelative: true }}
      />
    </LocaleProvider>
  );
}

describe("MenuCard chart refresh", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    tauriMocks.claudeAccountsList.mockResolvedValue([]);
    tauriMocks.getLocaleStrings.mockResolvedValue(buildBundle({}));
    tauriMocks.getDeepSeekPricingStatus.mockResolvedValue(null);
    tauriMocks.getProviderChartData.mockResolvedValue({
      providerId: "claude",
      costHistory: [],
      tokensHistory: [],
      creditsHistory: [],
      usageBreakdown: [],
      localUsage: null,
    });
  });

  it("re-reads chart data only when the local day changes across an update", async () => {
    vi.useFakeTimers({ toFake: ["Date"] });
    try {
      vi.setSystemTime(new Date(2026, 9, 7, 23, 0, 0));
      const { rerender } = render(card("2026-10-07T23:00:00Z"));
      await waitFor(() => expect(tauriMocks.getProviderChartData).toHaveBeenCalledTimes(1));

      // Plain rerender and a same-day provider update do not refetch.
      rerender(card("2026-10-07T23:00:00Z"));
      vi.setSystemTime(new Date(2026, 9, 7, 23, 30, 0));
      rerender(card("2026-10-07T23:30:00Z"));
      expect(tauriMocks.getProviderChartData).toHaveBeenCalledTimes(1);

      // The next update lands after local midnight; Today must be re-read.
      vi.setSystemTime(new Date(2026, 9, 8, 0, 5, 0));
      rerender(card("2026-10-08T00:05:00Z"));
      await waitFor(() => expect(tauriMocks.getProviderChartData).toHaveBeenCalledTimes(2));
    } finally {
      vi.useRealTimers();
    }
  });

  it("refreshes shared cost data on receipt even when the sender's quota timestamp is unchanged", async () => {
    const { rerender } = render(card("unchanged", { sourceLabel: "peer:home:1|account", peerReceivedAt: 100 }));
    await waitFor(() => expect(tauriMocks.getProviderChartData).toHaveBeenCalledTimes(1));
    rerender(card("unchanged", { sourceLabel: "peer:home:1|account", peerReceivedAt: 130 }));
    await waitFor(() => expect(tauriMocks.getProviderChartData).toHaveBeenCalledTimes(2));
  });
});
