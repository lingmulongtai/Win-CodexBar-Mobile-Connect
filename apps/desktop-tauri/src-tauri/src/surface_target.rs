use serde::{Deserialize, Serialize};

use crate::surface::SurfaceMode;

// Must mirror the frontend `SettingsTabId` union
// (apps/desktop-tauri/src/types/bridge.ts) and `TAB_META` in
// apps/desktop-tauri/src/surfaces/settings/settingsTabs.ts.
const SETTINGS_TAB_IDS: &[&str] = &[
    "general",
    "providers",
    "notifications",
    "menuBar",
    "menu",
    "usageSpend",
    "mobileConnect",
    "discord",
    "advanced",
    "hooks",
    "about",
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum SurfaceTarget {
    #[default]
    Summary,
    Dashboard,
    Provider {
        #[serde(rename = "providerId")]
        provider_id: String,
    },
    Settings {
        tab: String,
    },
}

impl SurfaceTarget {
    #[allow(
        dead_code,
        reason = "surface target type reserved for future window management integration"
    )]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "summary" => Some(Self::Summary),
            "dashboard" => Some(Self::Dashboard),
            _ => {
                if let Some(provider_id) = s.strip_prefix("provider:")
                    && !provider_id.is_empty()
                {
                    return Some(Self::Provider {
                        provider_id: provider_id.to_string(),
                    });
                }

                if let Some(tab) = s.strip_prefix("settings:")
                    && !tab.is_empty()
                {
                    return Some(Self::Settings {
                        tab: tab.to_string(),
                    });
                }

                None
            }
        }
    }

    pub fn default_for_mode(mode: SurfaceMode) -> Self {
        match mode {
            SurfaceMode::Hidden | SurfaceMode::TrayPanel => Self::Summary,
            SurfaceMode::PopOut => Self::Dashboard,
            SurfaceMode::Settings => Self::Settings {
                tab: "general".into(),
            },
        }
    }

    pub fn mode(&self) -> SurfaceMode {
        match self {
            Self::Summary => SurfaceMode::TrayPanel,
            Self::Dashboard | Self::Provider { .. } => SurfaceMode::PopOut,
            Self::Settings { .. } => SurfaceMode::Settings,
        }
    }
}

/// A settings tab id, or `providers:<cli_name>` for one provider's pane in
/// the Settings sidebar (frontend `parseSettingsTabRequest`). Only the exact
/// canonical `cli_name` is accepted, not CLI aliases such as `openai`.
pub fn is_supported_settings_tab(tab: &str) -> bool {
    if let Some(provider) = tab.strip_prefix("providers:") {
        return codexbar::core::ProviderId::from_cli_name(provider)
            .is_some_and(|id| id.cli_name() == provider);
    }
    SETTINGS_TAB_IDS.contains(&tab)
}

#[cfg(test)]
mod tests {
    use super::{SurfaceTarget, is_supported_settings_tab};
    use serde_json::json;

    #[test]
    fn parse_provider_target() {
        let target = SurfaceTarget::parse("provider:codex").unwrap();
        assert_eq!(
            target,
            SurfaceTarget::Provider {
                provider_id: "codex".into()
            }
        );
    }

    #[test]
    fn parse_settings_about_target() {
        let target = SurfaceTarget::parse("settings:about").unwrap();
        assert_eq!(
            target,
            SurfaceTarget::Settings {
                tab: "about".into()
            }
        );
    }

    #[test]
    fn serialize_provider_target_for_bridge() {
        let value = serde_json::to_value(SurfaceTarget::Provider {
            provider_id: "codex".into(),
        })
        .unwrap();

        assert_eq!(value, json!({ "kind": "provider", "providerId": "codex" }));
    }

    #[test]
    fn deserialize_settings_target_from_bridge() {
        let target: SurfaceTarget =
            serde_json::from_value(json!({ "kind": "settings", "tab": "general" })).unwrap();

        assert_eq!(
            target,
            SurfaceTarget::Settings {
                tab: "general".into()
            }
        );
    }

    #[test]
    fn target_mode_matches_surface_mode() {
        assert_eq!(
            SurfaceTarget::Summary.mode(),
            crate::surface::SurfaceMode::TrayPanel
        );
        assert_eq!(
            SurfaceTarget::Dashboard.mode(),
            crate::surface::SurfaceMode::PopOut
        );
        assert_eq!(
            SurfaceTarget::Provider {
                provider_id: "claude".into()
            }
            .mode(),
            crate::surface::SurfaceMode::PopOut
        );
        assert_eq!(
            SurfaceTarget::Settings {
                tab: "about".into()
            }
            .mode(),
            crate::surface::SurfaceMode::Settings
        );
    }

    #[test]
    fn supported_settings_tabs_match_shell_tabs() {
        assert!(is_supported_settings_tab("mobileConnect"));
        assert!(is_supported_settings_tab("discord"));
        assert!(is_supported_settings_tab("menuBar"));
        assert!(is_supported_settings_tab("hooks"));
        assert!(is_supported_settings_tab("about"));
        assert!(!is_supported_settings_tab("apiKeys"));
        assert!(!is_supported_settings_tab("security"));
    }

    #[test]
    fn provider_pane_tabs_need_a_known_provider() {
        assert!(is_supported_settings_tab("providers:codex"));
        assert!(is_supported_settings_tab("providers:claude"));
        assert!(!is_supported_settings_tab("providers:nope"));
        assert!(!is_supported_settings_tab("providers:"));
        assert!(!is_supported_settings_tab("providers:openai"));
        assert!(!is_supported_settings_tab("apiKeys"));
    }

    #[test]
    fn every_provider_catalog_id_is_a_valid_provider_pane_tab() {
        // The Settings catalog sends `cli_name` as the provider id, and the
        // sidebar builds `providers:<id>` from it.
        for id in codexbar::core::ProviderId::all() {
            let tab = format!("providers:{}", id.cli_name());
            assert!(is_supported_settings_tab(&tab), "{tab} rejected");
        }
    }
}
