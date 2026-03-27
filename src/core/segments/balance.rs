use super::{Segment, SegmentData};
use crate::config::{InputData, SegmentId};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// API response structures
#[derive(Debug, Deserialize)]
struct SubscriptionApiResponse {
    success: bool,
    data: Option<SubscriptionData>,
    #[allow(dead_code)]
    message: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SubscriptionData {
    subscriptions: Vec<SubscriptionWrapper>,
}

#[derive(Debug, Deserialize)]
struct SubscriptionWrapper {
    subscription: SubscriptionInfo,
}

#[derive(Debug, Deserialize)]
struct SubscriptionInfo {
    amount_total: u64,
    amount_used: u64,
    upgrade_group: String,
    end_time: i64,
}

// Cache structure
#[derive(Debug, Serialize, Deserialize)]
struct BalanceCache {
    remaining: f64,
    used: f64,
    total: f64,
    plan_name: String,
    expire_date: String,
    cached_at: String,
}

#[derive(Default)]
pub struct BalanceSegment;

impl BalanceSegment {
    pub fn new() -> Self {
        Self
    }

    fn get_cache_path() -> Option<std::path::PathBuf> {
        let home = dirs::home_dir()?;
        Some(
            home.join(".claude")
                .join("ccline")
                .join(".balance_cache.json"),
        )
    }

    fn load_cache(&self) -> Option<BalanceCache> {
        let cache_path = Self::get_cache_path()?;
        if !cache_path.exists() {
            return None;
        }
        let content = std::fs::read_to_string(&cache_path).ok()?;
        serde_json::from_str(&content).ok()
    }

    fn save_cache(&self, cache: &BalanceCache) {
        if let Some(cache_path) = Self::get_cache_path() {
            if let Some(parent) = cache_path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            if let Ok(json) = serde_json::to_string_pretty(cache) {
                let _ = std::fs::write(&cache_path, json);
            }
        }
    }

    fn is_cache_valid(&self, cache: &BalanceCache, cache_duration: u64) -> bool {
        if let Ok(cached_at) = DateTime::parse_from_rfc3339(&cache.cached_at) {
            let now = Utc::now();
            let elapsed = now.signed_duration_since(cached_at.with_timezone(&Utc));
            elapsed.num_seconds() < cache_duration as i64
        } else {
            false
        }
    }

    fn get_proxy_from_settings() -> Option<String> {
        let home = std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .ok()?;
        let settings_path = format!("{}/.claude/settings.json", home);

        let content = std::fs::read_to_string(&settings_path).ok()?;
        let settings: serde_json::Value = serde_json::from_str(&content).ok()?;

        settings
            .get("env")?
            .get("HTTPS_PROXY")
            .or_else(|| settings.get("env")?.get("HTTP_PROXY"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    }

    fn fetch_balance(
        &self,
        api_base_url: &str,
        access_token: &str,
        user_id: &str,
        timeout_secs: u64,
    ) -> Option<(f64, f64, f64, String, String)> {
        let url = format!("{}/api/subscription/self", api_base_url.trim_end_matches('/'));

        let agent = if let Some(proxy_url) = Self::get_proxy_from_settings() {
            if let Ok(proxy) = ureq::Proxy::new(&proxy_url) {
                ureq::Agent::config_builder()
                    .proxy(Some(proxy))
                    .build()
                    .new_agent()
            } else {
                ureq::Agent::new_with_defaults()
            }
        } else {
            ureq::Agent::new_with_defaults()
        };

        let response = agent
            .get(&url)
            .header("Content-Type", "application/json")
            .header("Authorization", &format!("Bearer {}", access_token))
            .header("New-Api-User", user_id)
            .config()
            .timeout_global(Some(std::time::Duration::from_secs(timeout_secs)))
            .build()
            .call()
            .ok()?;

        let api_response: SubscriptionApiResponse = response.into_body().read_json().ok()?;

        if !api_response.success {
            return None;
        }

        let data = api_response.data?;
        let subs = &data.subscriptions;

        if subs.is_empty() {
            return None;
        }

        let mut total_amount: u64 = 0;
        let mut total_used: u64 = 0;
        let mut names: Vec<String> = Vec::new();
        let mut latest_end_time: i64 = 0;

        for sub in subs {
            let s = &sub.subscription;
            total_amount += s.amount_total;
            total_used += s.amount_used;
            if !names.contains(&s.upgrade_group) {
                names.push(s.upgrade_group.clone());
            }
            if s.end_time > latest_end_time {
                latest_end_time = s.end_time;
            }
        }

        let remaining = (total_amount - total_used) as f64 / 500000.0;
        let used = total_used as f64 / 500000.0;
        let total = total_amount as f64 / 500000.0;
        let plan_name = names.join("+");

        // Format expire date as MM-DD
        let expire_date = if latest_end_time > 0 {
            let dt = chrono::DateTime::from_timestamp(latest_end_time, 0)
                .unwrap_or_default()
                .with_timezone(&chrono::Local);
            dt.format("%m-%d").to_string()
        } else {
            "?".to_string()
        };

        Some((remaining, used, total, plan_name, expire_date))
    }

}

impl Segment for BalanceSegment {
    fn collect(&self, _input: &InputData) -> Option<SegmentData> {
        // Load config to get segment options
        let config = crate::config::Config::load().ok()?;
        let segment_config = config
            .segments
            .iter()
            .find(|s| s.id == SegmentId::Balance);

        let api_base_url = segment_config
            .and_then(|sc| sc.options.get("api_base_url"))
            .and_then(|v| v.as_str())
            .unwrap_or("https://synai996.space");

        let access_token = segment_config
            .and_then(|sc| sc.options.get("access_token"))
            .and_then(|v| v.as_str())
            .unwrap_or("");

        let user_id = segment_config
            .and_then(|sc| sc.options.get("user_id"))
            .and_then(|v| v.as_str())
            .unwrap_or("1");

        let cache_duration = segment_config
            .and_then(|sc| sc.options.get("cache_duration"))
            .and_then(|v| v.as_u64())
            .unwrap_or(300);

        let timeout = segment_config
            .and_then(|sc| sc.options.get("timeout"))
            .and_then(|v| v.as_u64())
            .unwrap_or(3);

        if access_token.is_empty() {
            return None;
        }

        // Check cache first
        let cached_data = self.load_cache();
        let use_cached = cached_data
            .as_ref()
            .map(|cache| self.is_cache_valid(cache, cache_duration))
            .unwrap_or(false);

        let (remaining, used, total, plan_name, expire_date) = if use_cached {
            let cache = cached_data.unwrap();
            (cache.remaining, cache.used, cache.total, cache.plan_name, cache.expire_date)
        } else {
            match self.fetch_balance(api_base_url, access_token, user_id, timeout) {
                Some((remaining, used, total, plan_name, expire_date)) => {
                    let cache = BalanceCache {
                        remaining,
                        used,
                        total,
                        plan_name: plan_name.clone(),
                        expire_date: expire_date.clone(),
                        cached_at: Utc::now().to_rfc3339(),
                    };
                    self.save_cache(&cache);
                    (remaining, used, total, plan_name, expire_date)
                }
                None => {
                    // Fallback to stale cache if API fails
                    if let Some(cache) = cached_data {
                        (cache.remaining, cache.used, cache.total, cache.plan_name, cache.expire_date)
                    } else {
                        return None;
                    }
                }
            }
        };

        let remaining_pct = if total > 0.0 {
            (remaining / total) * 100.0
        } else {
            0.0
        };

        // Color circle based on remaining percentage
        let status_dot = if remaining_pct > 60.0 {
            "🟢"
        } else if remaining_pct > 40.0 {
            "🟡"
        } else if remaining_pct > 20.0 {
            "🟠"
        } else {
            "🔴"
        };

        // Format: 🟡 51% · 💸 Used: $359.41 · 💰 Left: $391 · 📅 Expires: 04-06
        let pct_display = remaining_pct.round() as u32;
        let primary = format!(
            "{} {}% · 💸 Used: ${:.2} · 💰 Left: ${:.0} · 📅 Expires: {}",
            status_dot, pct_display, used, remaining, expire_date
        );
        let secondary = String::new();

        let mut metadata = HashMap::new();
        metadata.insert("remaining".to_string(), format!("{:.2}", remaining));
        metadata.insert("used".to_string(), format!("{:.2}", used));
        metadata.insert("total".to_string(), format!("{:.2}", total));
        metadata.insert("remaining_pct".to_string(), format!("{:.1}", remaining_pct));
        metadata.insert("plan_name".to_string(), plan_name);

        Some(SegmentData {
            primary,
            secondary,
            metadata,
        })
    }

    fn id(&self) -> SegmentId {
        SegmentId::Balance
    }
}
