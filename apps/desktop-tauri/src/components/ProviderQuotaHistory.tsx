import { useEffect, useState } from "react";
import type { ProviderQuotaHistorySeries, ProviderUsageSnapshot } from "../types/bridge";
import type { LocaleKey } from "../i18n/keys";
import { getProviderQuotaHistory } from "../lib/tauri";

export default function ProviderQuotaHistory({ provider, t, onLayoutChange, now = Date.now() }: {
  provider: ProviderUsageSnapshot;
  t: (key: LocaleKey) => string;
  onLayoutChange?: () => void;
  now?: number;
}) {
  const [days, setDays] = useState(7);
  const [result, setResult] = useState<{ key: string; series: ProviderQuotaHistorySeries[] } | null>(null);
  const requestKey = `${provider.sourceLabel}|${provider.providerId}|${provider.updatedAt}|${provider.accountEmail ?? ""}`;
  useEffect(() => {
    let current = true;
    void getProviderQuotaHistory(provider.providerId, provider.updatedAt, provider.sourceLabel).then(series => {
      if (current) setResult({ key: requestKey, series });
    }).catch(() => { if (current) setResult({ key: requestKey, series: [] }); });
    return () => { current = false; };
  }, [provider.providerId, provider.updatedAt, requestKey]);
  useEffect(() => { onLayoutChange?.(); }, [result, days, onLayoutChange]);
  const end = Math.floor(now / 1000);
  const start = end - days * 86400;
  const series = (result?.key === requestKey ? result.series : []).map(item => ({
    ...item,
    samples: item.samples.filter(sample => Number.isFinite(sample.t) && Number.isFinite(sample.u)
      && sample.t >= start && sample.t <= end && sample.u >= 0 && sample.u <= 1),
  })).filter(item => item.samples.length > 0);
  const x = (t: number) => 4 + (t - start) / (end - start) * 292;
  const y = (u: number) => 4 + u * 68;
  const date = (t: number) => new Date(t * 1000).toLocaleDateString([], { month: "short", day: "numeric" });
  return <section className="quota-archive" aria-label={t("QuotaArchiveTitle")}>
    <div className="quota-archive__heading">
      <strong>{t("QuotaArchiveTitle")}</strong>
      <div role="group" aria-label={t("CostPeriodHistoryWindow")}>
        {[7, 30].map(value => <button key={value} type="button" className="credential-btn" aria-pressed={days === value}
          onClick={() => setDays(value)}>{t(value === 7 ? "QuotaArchiveSevenDays" : "QuotaArchiveThirtyDays")}</button>)}
      </div>
    </div>
    {series.map(item => <div className="quota-burndown-chart" key={item.label}>
      <div className="quota-burndown-chart__caption"><span>{item.label}</span>
        <span>{`${Math.round((1 - item.samples[item.samples.length - 1].u) * 100)}% ${t("PanelLeftSuffix")}`}</span></div>
      <svg viewBox="0 0 300 76" role="img" aria-label={`${item.label} · ${t("QuotaArchiveTitle")}`}>
        <line className="quota-burndown-chart__grid" x1="4" y1="72" x2="296" y2="72" />
        {/* The shared projection is sampled. Bars show real observations without
            inferring continuity across omitted resets or offline periods. */}
        {item.samples.map(point => <line key={point.t} className="quota-burndown-chart__observed"
          x1={x(point.t)} y1="72" x2={x(point.t)} y2={y(point.u)} />)}
        {item.samples.map(point => <circle key={point.t} className="quota-burndown-chart__point" cx={x(point.t)} cy={y(point.u)} r="2" />)}
      </svg>
      <div className="quota-burndown-chart__axis"><span>{date(start)}</span><span>{date(end)}</span></div>
    </div>)}
    {series.length === 0 && <p className="menu-surface__hint">{t("UsageSpendTrendNoData")}</p>}
    <p className="menu-surface__hint">{t("QuotaArchiveHelp")}</p>
  </section>;
}
