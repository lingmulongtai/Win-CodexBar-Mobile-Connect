use super::*;
use crate::core::{
    CodexPriorityOverlay, CodexSourceUsageRow, CodexUsageRecord, CostUsagePricing, row_priced_model,
};
use std::collections::BTreeMap;

type DayModels = HashMap<String, HashMap<String, Vec<i64>>>;

pub(super) fn rebuild_cache_days(cache: &mut CostUsageCache) {
    cache.days.clear();
    let files = &cache.files;
    cache.codex_fork_rows.retain(|path, _| {
        files
            .get(path)
            .is_some_and(|usage| !usage.codex_unresolved_fork_parent)
    });
    let cursor = cache.codex_priority_turns_cursor.as_ref();
    for (path, usage) in &cache.files {
        let rows = cache
            .codex_fork_rows
            .get(path)
            .map(Vec::as_slice)
            .filter(|rows| {
                day_model_token_totals(&days_from_codex_source_rows(rows))
                    == day_model_token_totals(&usage.days)
            })
            .or_else(|| {
                cache
                    .codex_source_rows
                    .get(path)
                    .map(|source| source.rows.as_slice())
            });
        let overlaid = cursor
            .and_then(|cursor| cursor.overlay_for_file(Path::new(path)))
            .zip(rows)
            .and_then(|(overlay, rows)| priority_days(rows, &usage.days, &overlay));
        let file_days = overlaid.as_ref().unwrap_or(&usage.days);
        for (day, models) in file_days {
            let day_entry = cache.days.entry(day.clone()).or_default();
            for (model, packed) in models {
                let dest = day_entry
                    .entry(model.clone())
                    .or_insert_with(|| vec![0, 0, 0]);
                merge_packed(dest, packed);
            }
        }
    }
    let saved: Vec<_> = super::saved_history::active_rows(cache)
        .map(|(_, rows)| days_from_codex_source_rows(rows))
        .collect();
    for days in saved {
        for (day, models) in days {
            for (model, packed) in models {
                let target = cache
                    .days
                    .entry(day.clone())
                    .or_default()
                    .entry(model)
                    .or_insert_with(|| vec![0, 0, 0]);
                merge_packed(target, &packed);
            }
        }
    }
}

fn merge_packed(dest: &mut Vec<i64>, packed: &[i64]) {
    if dest.len() < 3 {
        dest.resize(3, 0);
    }

    let had_core_tokens = dest[0] != 0 || dest[1] != 0 || dest[2] != 0;
    let source_input = packed.first().copied().unwrap_or(0);
    let source_cached = packed.get(1).copied().unwrap_or(0);
    let source_output = packed.get(2).copied().unwrap_or(0);
    let source_has_tokens = source_input != 0 || source_cached != 0 || source_output != 0;
    let source_reasoning = packed
        .get(3)
        .copied()
        .map(|reasoning| reasoning.max(0).min(source_output.max(0)));

    dest[0] = dest[0].saturating_add(source_input);
    dest[1] = dest[1].saturating_add(source_cached);
    dest[2] = dest[2].saturating_add(source_output);

    if !source_has_tokens {
        return;
    }

    if !had_core_tokens {
        match source_reasoning {
            Some(reasoning) => {
                if dest.len() >= 4 {
                    dest[3] = reasoning.min(dest[2].max(0));
                } else {
                    dest.push(reasoning.min(dest[2].max(0)));
                }
            }
            None => dest.truncate(3),
        }
        return;
    }

    match (dest.get(3).copied(), source_reasoning) {
        (Some(previous), Some(reasoning)) => {
            let merged = previous.saturating_add(reasoning).min(dest[2].max(0));
            dest[3] = merged;
        }
        _ => dest.truncate(3),
    }
}

/// Replace day-aggregate prices with request-level prices only when retained
/// rows prove the model and token breakdown for the entire source file.
/// An unverified portion is left explicitly unpriced, never assigned a tier
/// from the sum of unrelated requests.
struct CodexRequestPrices {
    total: CostSummary,
    daily: HashMap<String, CostSummary>,
    missing: HashMap<String, HashSet<String>>,
}

fn source_request_prices(cache: &CostUsageCache, range: &CostUsageDayRange) -> CodexRequestPrices {
    let mut data = CodexRequestPrices {
        total: CostSummary::default(),
        daily: HashMap::new(),
        missing: HashMap::new(),
    };
    for (path, usage) in &cache.files {
        let relevant: Vec<_> = usage
            .days
            .iter()
            .filter(|(day, _)| {
                CostUsageDayRange::is_in_range(day, &range.since_key, &range.until_key)
            })
            .collect();
        if relevant.is_empty() {
            continue;
        }
        let rows = cache
            .codex_fork_rows
            .get(path)
            .map(Vec::as_slice)
            .filter(|rows| {
                day_model_token_totals(&days_from_codex_source_rows(rows))
                    == day_model_token_totals(&usage.days)
            })
            .or_else(|| {
                cache
                    .codex_source_rows
                    .get(path)
                    .map(|source| source.rows.as_slice())
            });
        // Compact evidence is from the same parser pass as these token totals.
        let valid = rows.is_some_and(|rows| {
            !usage.codex_unresolved_fork_parent
                && day_model_token_totals(&days_from_codex_source_rows(rows))
                    == day_model_token_totals(&usage.days)
        });
        if !valid {
            for (day, models) in relevant {
                for (model, packed) in models {
                    let input = packed.first().copied().unwrap_or(0).max(0) as u64;
                    let cached = packed.get(1).copied().unwrap_or(0).max(0) as u64;
                    let output = packed.get(2).copied().unwrap_or(0).max(0) as u64;
                    let date = CostUsageDayRange::parse_day_key(day);
                    let price = |i, c, o| {
                        CostUsagePricing::codex_day_aggregate_cost_usd(model, i, c, o, date)
                    };
                    let linear = price(1, 0, 0).zip(price(1, 1, 0)).zip(price(0, 0, 1)).map(
                        |((i, c), o)| {
                            (input.saturating_sub(cached.min(input)) as f64) * i
                                + (cached.min(input) as f64) * c
                                + (output as f64) * o
                        },
                    );
                    if price(input, cached, output)
                        .zip(linear)
                        .is_some_and(|(a, b)| (a - b).abs() < 1e-9)
                    {
                        for target in [&mut data.total, data.daily.entry(day.clone()).or_default()]
                        {
                            if let Some(cost) =
                                crate::codex_costs::add_codex_packed_tokens_to_summary(
                                    target, model, packed, date,
                                )
                            {
                                target.total_cost_usd += cost;
                            }
                        }
                    } else {
                        data.missing
                            .entry(day.clone())
                            .or_default()
                            .insert(model.clone());
                    }
                }
            }
            continue;
        }
        let Some(rows) = rows else { continue };
        let overlay = cache
            .codex_priority_turns_cursor
            .as_ref()
            .and_then(|cursor| cursor.overlay_for_file(Path::new(path)));
        for row in rows {
            if !CostUsageDayRange::is_in_range(&row.day_key, &range.since_key, &range.until_key) {
                continue;
            }
            let model = row_priced_model(row, overlay.as_ref())
                .unwrap_or_else(|| CostUsagePricing::CODEX_UNATTRIBUTED_MODEL.to_string());
            if !request_price_supported(Path::new(path), row, &model) {
                data.missing
                    .entry(row.day_key.clone())
                    .or_default()
                    .insert(model);
                continue;
            }
            let mut packed = vec![row.input, row.cached, row.output];
            if let Some(reasoning) = row.reasoning {
                packed.push(reasoning);
            }
            for priced in [
                &mut data.total,
                data.daily.entry(row.day_key.clone()).or_default(),
            ] {
                if let Some(cost) = crate::codex_costs::add_codex_packed_tokens_to_summary(
                    priced,
                    &model,
                    &packed,
                    CostUsageDayRange::parse_day_key(&row.day_key),
                ) {
                    priced.total_cost_usd += cost;
                }
            }
        }
    }
    for (_, rows) in super::saved_history::active_rows(cache) {
        for row in rows {
            if !CostUsageDayRange::is_in_range(&row.day_key, &range.since_key, &range.until_key) {
                continue;
            }
            // The source file is gone, so a nonlinear aggregate cannot be
            // verified as one request. Keep its tokens but report it unpriced.
            let model = row_priced_model(row, None)
                .unwrap_or_else(|| CostUsagePricing::CODEX_UNATTRIBUTED_MODEL.to_string());
            if !linear_price_matches(&model, row.input, row.cached, row.output, &row.day_key) {
                data.missing
                    .entry(row.day_key.clone())
                    .or_default()
                    .insert(model);
                continue;
            }
            let mut packed = vec![row.input, row.cached, row.output];
            if let Some(reasoning) = row.reasoning {
                packed.push(reasoning);
            }
            for target in [
                &mut data.total,
                data.daily.entry(row.day_key.clone()).or_default(),
            ] {
                if let Some(cost) = crate::codex_costs::add_codex_packed_tokens_to_summary(
                    target,
                    &model,
                    &packed,
                    CostUsageDayRange::parse_day_key(&row.day_key),
                ) {
                    target.total_cost_usd += cost;
                }
            }
        }
    }
    data
}

pub(crate) fn price_summary_from_source_rows(
    summary: &mut CostSummary,
    cache: &CostUsageCache,
    range: &CostUsageDayRange,
) {
    let data = source_request_prices(cache, range);
    apply_request_prices(summary, data.total, data.missing.values().flatten());
    if cache.codex_scan_incomplete && !codex_current_window_is_established(cache, range) {
        summary.model_pricing_completeness = ModelPricingCompleteness::Partial {
            unpriced_models: summary.by_model_tokens.keys().cloned().collect(),
        };
    }
}

fn apply_request_prices<'a>(
    summary: &mut CostSummary,
    priced: CostSummary,
    missing_models: impl Iterator<Item = &'a String>,
) {
    let mut missing = HashSet::new();
    for model in missing_models {
        let base = CostUsagePricing::codex_fast_base_model(model);
        for existing in summary.by_model_tokens.keys() {
            if CostUsagePricing::codex_fast_base_model(existing) == base {
                missing.insert(existing.clone());
            }
        }
    }
    for (model, counts) in &summary.by_model_tokens {
        if priced.by_model_tokens.get(model) != Some(counts) {
            missing.insert(model.clone());
        }
    }
    summary.total_cost_usd = priced.total_cost_usd;
    summary.by_model = priced.by_model;
    summary.by_speed = priced.by_speed;
    summary.unknown_models = priced.unknown_models;
    let mut unpriced = match priced.model_pricing_completeness {
        ModelPricingCompleteness::Complete => Vec::new(),
        ModelPricingCompleteness::Partial { unpriced_models } => unpriced_models,
    };
    unpriced.extend(missing);
    unpriced.sort();
    unpriced.dedup();
    summary.model_pricing_completeness = if unpriced.is_empty() {
        ModelPricingCompleteness::Complete
    } else {
        ModelPricingCompleteness::Partial {
            unpriced_models: unpriced,
        }
    };
}

pub(crate) fn daily_prices_from_source_rows(
    cache: &CostUsageCache,
    range: &CostUsageDayRange,
) -> HashMap<String, Option<f64>> {
    if cache.codex_scan_incomplete {
        return cache
            .days
            .keys()
            .filter(|day| CostUsageDayRange::is_in_range(day, &range.since_key, &range.until_key))
            .map(|day| (day.clone(), None))
            .collect();
    }
    let mut data = source_request_prices(cache, range);
    let mut result = HashMap::new();
    for (day, models) in &cache.days {
        if !CostUsageDayRange::is_in_range(day, &range.since_key, &range.until_key) {
            continue;
        }
        let Some(date) = CostUsageDayRange::parse_day_key(day) else {
            continue;
        };
        let one_day = HashMap::from([(day.clone(), models.clone())]);
        let mut expected = CostSummary::default();
        add_codex_days_map_to_summary(&mut expected, &one_day, &CostUsageDayRange::new(date, date));
        let priced = data.daily.remove(day).unwrap_or_default();
        let missing = data.missing.get(day);
        apply_request_prices(&mut expected, priced, missing.into_iter().flatten());
        result.insert(
            day.clone(),
            (!expected.model_pricing_completeness.is_partial()).then_some(expected.total_cost_usd),
        );
    }
    result
}

fn day_model_token_totals(days: &DayModels) -> BTreeMap<(&str, &str), (i64, i64, i64)> {
    let mut totals = BTreeMap::new();
    for (day, models) in days {
        for (model, packed) in models {
            totals.insert(
                (day.as_str(), model.as_str()),
                (
                    packed.first().copied().unwrap_or(0),
                    packed.get(1).copied().unwrap_or(0),
                    packed.get(2).copied().unwrap_or(0),
                ),
            );
        }
    }
    totals
}

/// Day totals for one file with Priority trace evidence applied, or `None`
/// when no row is affected or the retained source rows no longer match the
/// file's parsed day totals (a stale row cache must never replace newer data).
fn priority_days(
    rows: &[CodexSourceUsageRow],
    parsed_days: &DayModels,
    overlay: &CodexPriorityOverlay<'_>,
) -> Option<DayModels> {
    let plain = days_from_codex_source_rows_with_priority(rows, None);
    if day_token_totals(&plain) != day_token_totals(parsed_days) {
        return None;
    }
    let overlaid = days_from_codex_source_rows_with_priority(rows, Some(overlay));
    (overlaid != plain).then_some(overlaid)
}

fn day_token_totals(days: &DayModels) -> BTreeMap<&str, (i64, i64, i64)> {
    let mut totals = BTreeMap::new();
    for (day, models) in days {
        let entry: &mut (i64, i64, i64) = totals.entry(day.as_str()).or_default();
        for packed in models.values() {
            entry.0 = entry.0.saturating_add(packed.first().copied().unwrap_or(0));
            entry.1 = entry.1.saturating_add(packed.get(1).copied().unwrap_or(0));
            entry.2 = entry.2.saturating_add(packed.get(2).copied().unwrap_or(0));
        }
    }
    totals
}

pub(super) fn days_from_codex_source_rows(rows: &[CodexSourceUsageRow]) -> DayModels {
    days_from_codex_source_rows_with_priority(rows, None)
}

fn days_from_codex_source_rows_with_priority(
    rows: &[CodexSourceUsageRow],
    overlay: Option<&CodexPriorityOverlay<'_>>,
) -> DayModels {
    let mut days: DayModels = HashMap::new();
    for row in rows {
        let model = row_priced_model(row, overlay)
            .unwrap_or_else(|| CostUsagePricing::CODEX_UNATTRIBUTED_MODEL.to_string());
        let record = CodexUsageRecord {
            day_key: row.day_key.clone(),
            timestamp: row.timestamp,
            model,
            input: row.input,
            cached: row.cached,
            output: row.output,
            reasoning: row.reasoning,
            turn_id: row.turn_id.clone(),
        };
        let packed = days
            .entry(record.day_key.clone())
            .or_default()
            .entry(record.model.clone())
            .or_default();
        JsonlScanner::merge_codex_record_into_packed(packed, &record);
    }
    days
}

/// Cumulative-only deltas may span several requests. Nonlinear prices require
/// the matching last-request counters, read at the recorded offset (at most 64 KiB).
fn request_usage_matches(path: &Path, row: &CodexSourceUsageRow) -> bool {
    use std::io::{Read, Seek, SeekFrom};
    let Ok(end) = u64::try_from(row.source_end_offset) else {
        return false;
    };
    let start = end.saturating_sub(64 * 1024);
    let Some(bytes) = (|| {
        let mut file = fs::File::open(path).ok()?;
        file.seek(SeekFrom::Start(start)).ok()?;
        let mut bytes = Vec::new();
        file.take(end - start).read_to_end(&mut bytes).ok()?;
        Some(bytes)
    })() else {
        return false;
    };
    let Some(line) = bytes
        .split(|b| *b == b'\n')
        .rev()
        .find(|line| !line.is_empty())
    else {
        return false;
    };
    let Ok(event) = serde_json::from_slice::<serde_json::Value>(line) else {
        return false;
    };
    let Some(last) = event.pointer("/payload/info/last_token_usage") else {
        return false;
    };
    let cached = last
        .get("cached_input_tokens")
        .or_else(|| last.get("cache_read_input_tokens"))
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(0);
    last.get("input_tokens").and_then(serde_json::Value::as_i64) == Some(row.input)
        && last
            .get("output_tokens")
            .and_then(serde_json::Value::as_i64)
            == Some(row.output)
        && cached == row.cached
}

fn linear_price_matches(model: &str, input: i64, cached: i64, output: i64, day: &str) -> bool {
    let input = u64::try_from(input.max(0)).unwrap_or(0);
    let cached = u64::try_from(cached.max(0)).unwrap_or(0).min(input);
    let output = u64::try_from(output.max(0)).unwrap_or(0);
    let date = CostUsageDayRange::parse_day_key(day);
    let price = |i, c, o| CostUsagePricing::codex_day_aggregate_cost_usd(model, i, c, o, date);
    let linear = price(1, 0, 0)
        .zip(price(1, 1, 0))
        .zip(price(0, 0, 1))
        .map(|((i, c), o)| (input - cached) as f64 * i + cached as f64 * c + output as f64 * o);
    price(input, cached, output)
        .zip(linear)
        .is_none_or(|(a, b)| (a - b).abs() < 1e-9)
}

pub(crate) fn request_price_supported(path: &Path, row: &CodexSourceUsageRow, model: &str) -> bool {
    linear_price_matches(model, row.input, row.cached, row.output, &row.day_key)
        || request_usage_matches(path, row)
}
