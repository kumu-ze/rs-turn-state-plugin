//! 打标管理的非敏感合同；上游票原文始终留在 Provider 内。

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TicketMode {
    #[default]
    Off,
    Manual,
    Auto,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TicketAccountPolicy {
    pub mode: TicketMode,
    pub target_length: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TicketSettings {
    pub enabled: bool,
    pub inject: bool,
    #[serde(default = "default_pool_enabled")]
    pub proxy_pool_enabled: bool,
    #[serde(default)]
    pub activity_only: bool,
    #[serde(default = "default_idle_seconds")]
    pub idle_seconds: u64,
    #[serde(default)]
    pub require_ticket: bool,
    pub plus_pro_length: usize,
    pub business_length: usize,
    pub default_length: usize,
    pub models: Vec<String>,
    pub interval_seconds: u64,
    #[serde(default)]
    pub manual_interval_seconds: u64,
    pub ttl_seconds: u64,
    pub refresh_before_seconds: u64,
    pub accounts: BTreeMap<String, TicketAccountPolicy>,
}

impl Default for TicketSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            inject: false,
            proxy_pool_enabled: true,
            activity_only: false,
            idle_seconds: default_idle_seconds(),
            require_ticket: false,
            plus_pro_length: 292,
            business_length: 332,
            default_length: 292,
            models: vec!["gpt-6-astra".into(), "gpt-5.6-sol".into()],
            interval_seconds: 60,
            manual_interval_seconds: 0,
            ttl_seconds: 3600,
            refresh_before_seconds: 600,
            accounts: BTreeMap::new(),
        }
    }
}

fn default_pool_enabled() -> bool {
    true
}

const fn default_idle_seconds() -> u64 {
    120
}

impl TicketSettings {
    /// 长度只是管理员规则，不代表上游对套餐或模型质量的承诺。
    pub fn target_length(&self, account_id: &str, plan: Option<&str>) -> usize {
        if let Some(length) = self.accounts.get(account_id).and_then(|p| p.target_length) {
            return length;
        }
        match plan.unwrap_or("").to_ascii_lowercase().as_str() {
            "plus" | "pro" => self.plus_pro_length,
            "business" | "team" => self.business_length,
            _ => self.default_length,
        }
    }

    pub fn validate(&self) -> bool {
        let length_ok = |n| (64..=4096).contains(&n);
        length_ok(self.plus_pro_length)
            && length_ok(self.business_length)
            && length_ok(self.default_length)
            && (10..=86400).contains(&self.interval_seconds)
            && self.manual_interval_seconds <= 86400
            && (10..=3600).contains(&self.idle_seconds)
            && (!self.require_ticket || self.inject)
            && (60..=3600).contains(&self.ttl_seconds)
            && self.refresh_before_seconds < self.ttl_seconds
            && !self.models.is_empty()
            && self.models.len() <= 8
            && self.models.iter().all(|m| {
                !m.is_empty()
                    && m.len() <= 128
                    && m.bytes()
                        .all(|c| c.is_ascii_alphanumeric() || b"-_.".contains(&c))
            })
            && self
                .models
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len()
                == self.models.len()
            && self.accounts.len() <= 10000
            && self.accounts.iter().all(|(id, p)| {
                !id.is_empty()
                    && id.len() <= 256
                    && !id.chars().any(char::is_control)
                    && p.target_length.is_none_or(length_ok)
            })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TicketResult {
    pub account_id: String,
    pub model: String,
    pub http_status: u16,
    pub length: usize,
    pub matched: bool,
    pub checked_at: u64,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TicketLog {
    pub id: String,
    pub account_name: String,
    pub trigger: TicketMode,
    #[serde(default)]
    pub continuous: bool,
    pub proxy_endpoint: String,
    #[serde(default)]
    pub proxy_name: String,
    pub target_length: usize,
    pub started_at: u64,
    pub duration_ms: u64,
    pub retry_at: Option<u64>,
    pub result: TicketResult,
    #[serde(default)]
    pub token_issued_at: Option<u64>,
    #[serde(default)]
    pub exit_sample: Option<TicketExitSample>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TicketExitSample {
    pub ip: Option<std::net::IpAddr>,
    pub checked_at: u64,
    pub message: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TicketExitProbe {
    pub revision: u64,
    pub proxy_id: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TicketAccountStatus {
    pub id: String,
    pub name: String,
    pub plan: Option<String>,
    pub eligible: bool,
    pub target_length: usize,
    pub policy: TicketAccountPolicy,
    pub models: Vec<TicketModelStatus>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TicketModelStatus {
    pub model: String,
    pub ready: bool,
    pub expires_at: Option<u64>,
    pub last_result: Option<TicketResult>,
    pub busy: bool,
    pub retry_at: Option<u64>,
    pub manual_retry_at: Option<u64>,
    pub continuous: Option<TicketContinuousStatus>,
    pub token_issued_at: Option<u64>,
    pub last_requested_at: Option<u64>,
    pub auto_paused: bool,
    pub blocked: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TicketContinuousStatus {
    pub interval_seconds: u64,
    pub next_probe_at: u64,
    pub proxy_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TicketPanel {
    pub revision: u64,
    pub settings: TicketSettings,
    pub proxy_configured: bool,
    pub proxy_count: usize,
    pub proxies: Vec<TicketProxyView>,
    pub accounts: Vec<TicketAccountStatus>,
    pub logs: Vec<TicketLog>,
    pub log_limit: usize,
    pub worker_checked_at: Option<u64>,
}

// 不派生 Debug，避免代理认证进入日志；None 保留，空串清除。
#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TicketUpdate {
    pub revision: u64,
    pub settings: TicketSettings,
    pub proxy_url: Option<String>,
    #[serde(default)]
    pub proxy_pool: Option<Vec<String>>,
    pub proxies: Option<Vec<TicketProxyInput>>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TicketProxyView {
    pub id: String,
    pub name: String,
    pub endpoint: String,
    pub has_authentication: bool,
    pub enabled: bool,
    pub concurrency: usize,
    pub in_flight: usize,
    pub exit_sample: Option<TicketExitSample>,
}

// URL 只允许写入，不派生 Debug；旧条目凭 revision + id 保留认证。
#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TicketProxyInput {
    pub id: Option<String>,
    pub name: String,
    pub url: Option<String>,
    pub saved_proxy_id: Option<String>,
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub concurrency: Option<usize>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TicketProbe {
    pub account_id: String,
    pub model: String,
    pub proxy_id: Option<String>,
    pub revision: Option<u64>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TicketContinuousInput {
    pub account_id: String,
    pub model: String,
    pub interval_seconds: Option<u64>,
    pub proxy_id: Option<String>,
    pub revision: Option<u64>,
}
