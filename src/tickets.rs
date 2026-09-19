//! 插件拥有打票策略、调度与持久化；宿主提供账号快照和受限网络探测。

use crate::bridge::{
    Account as ProviderAccount, ErrorKind as ProviderAdminErrorKind, HostBridge,
    ServiceError as ProviderAdminError,
};
use crate::model::*;
use base64::{
    Engine as _,
    engine::general_purpose::{URL_SAFE, URL_SAFE_NO_PAD},
};
use futures::future::BoxFuture;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Instant, SystemTime, UNIX_EPOCH},
};
use tokio::sync::{Mutex as AsyncMutex, OwnedMutexGuard, Semaphore};
use tokio_util::sync::CancellationToken;

const LOG_LIMIT: usize = 1000;
const MAX_PROBE_CONCURRENCY: usize = 12;
const EXIT_SAMPLE_TTL: u64 = 600;

#[derive(Clone, Serialize, Deserialize)]
struct Ticket {
    account_id: String,
    credential_revision: u64,
    #[serde(default)]
    auth_binding: Option<String>,
    model: String,
    state: String,
    expires_at: u64,
}

#[derive(Clone, Serialize, Deserialize)]
struct Persisted {
    revision: u64,
    settings: TicketSettings,
    #[serde(default)]
    proxy_url: String,
    #[serde(default)]
    proxy_pool: Vec<String>,
    #[serde(default)]
    proxy_names: Vec<String>,
    #[serde(default)]
    proxy_enabled: Vec<bool>,
    #[serde(default)]
    proxy_concurrency: Vec<usize>,
    tickets: Vec<Ticket>,
    #[serde(default)]
    logs: Vec<TicketLog>,
    #[serde(default)]
    cooldown_checkpoints: Vec<CooldownCheckpoint>,
}

#[derive(Clone, Serialize, Deserialize)]
struct CooldownCheckpoint {
    last: TicketResult,
    retry_at: Option<u64>,
    manual_retry_at: Option<u64>,
}

impl Default for Persisted {
    fn default() -> Self {
        Self {
            revision: 1,
            settings: TicketSettings::default(),
            proxy_url: String::new(),
            proxy_pool: Vec::new(),
            proxy_names: Vec::new(),
            proxy_enabled: Vec::new(),
            proxy_concurrency: Vec::new(),
            tickets: Vec::new(),
            logs: Vec::new(),
            cooldown_checkpoints: Vec::new(),
        }
    }
}

#[derive(Default)]
struct Observations {
    last: BTreeMap<(String, String), TicketResult>,
    retry: BTreeMap<String, u64>,
    manual_retry: BTreeMap<String, u64>,
    busy: BTreeMap<(String, String), usize>,
    proxy_busy: BTreeMap<String, usize>,
    proxy_cursor: usize,
    worker_checked_at: Option<u64>,
    continuous: BTreeMap<(String, String), ContinuousProbe>,
    last_requests: BTreeMap<(String, String), u64>,
    exit_samples: BTreeMap<String, TicketExitSample>,
}

#[derive(Clone)]
struct ContinuousProbe {
    id: uuid::Uuid,
    interval_seconds: u64,
    next_probe_at: u64,
    proxy: Option<String>,
    cancellation: CancellationToken,
}

impl Observations {
    fn retry_at(&self, id: &str, interval: u64, manual: bool) -> Option<u64> {
        let ordinary = self
            .last
            .iter()
            .filter(|((account, _), _)| account == id)
            .map(|(_, result)| result.checked_at.saturating_add(interval))
            .max()
            .unwrap_or(0);
        let retries = if manual {
            &self.manual_retry
        } else {
            &self.retry
        };
        Some(ordinary.max(retries.get(id).copied().unwrap_or(0))).filter(|time| *time > now())
    }
}

pub struct TicketService {
    host: HostBridge,
    path: PathBuf,
    data: Arc<Mutex<Persisted>>,
    observations: Mutex<Observations>,
    persistence: Arc<AsyncMutex<()>>,
    request_slot: Semaphore,
    exit_slot: Semaphore,
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |v| v.as_secs())
}
fn error(kind: ProviderAdminErrorKind) -> ProviderAdminError {
    ProviderAdminError::new(kind)
}
fn valid_state(state: &str, length: usize) -> bool {
    state.len() == length
        && state.starts_with("gAAAAA")
        && state
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"_-=".contains(&c))
}

// Fernet 的公开时间字段是创建时间；没有密钥不能验签，不能据此推导上游失效时间。
fn token_timestamp(state: &str) -> Option<u64> {
    if state.len() > 4096 {
        return None;
    }
    let raw = URL_SAFE
        .decode(state)
        .or_else(|_| URL_SAFE_NO_PAD.decode(state))
        .ok()?;
    if raw.len() < 73 || raw[0] != 0x80 || (raw.len() - 57) % 16 != 0 {
        return None;
    }
    let timestamp = u64::from_be_bytes(raw[1..9].try_into().ok()?);
    (timestamp <= now().saturating_add(300)).then_some(timestamp)
}

fn gated_model(model: &str) -> bool {
    let model = model.to_ascii_lowercase();
    ["gpt-6", "gpt-5.6"].iter().any(|prefix| {
        model
            .strip_prefix(prefix)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with('-') || rest.starts_with('.'))
    })
}
fn eligible(account: &ProviderAccount) -> bool {
    account.eligible
}
fn binding_matches(ticket: &Ticket, account: &ProviderAccount, binding: Option<&str>) -> bool {
    ticket
        .auth_binding
        .as_deref()
        .is_some_and(|expected| Some(expected) == binding)
        && account.eligible
}

impl TicketService {
    pub fn new(host: HostBridge, path: PathBuf) -> Result<Self, ProviderAdminError> {
        let mut data = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice::<Persisted>(&bytes)
                .map_err(|_| error(ProviderAdminErrorKind::Invalid))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Persisted::default(),
            Err(_) => return Err(error(ProviderAdminErrorKind::Internal)),
        };
        if data.proxy_pool.is_empty() && !data.proxy_url.is_empty() {
            data.proxy_pool = split_proxy_pool(&data.proxy_url);
        }
        if !data.settings.validate()
            || data
                .proxy_concurrency
                .iter()
                .any(|limit| !(1..=3).contains(limit))
            || data.proxy_pool.len() > 64
            || !data
                .proxy_pool
                .iter()
                .all(|p| !p.is_empty() && valid_proxy(p))
        {
            return Err(error(ProviderAdminErrorKind::Invalid));
        }
        data.tickets.retain(|t| {
            t.expires_at > now()
                && t.expires_at <= now() + 3600
                && valid_state(&t.state, t.state.len())
                && t.state.len() <= 4096
        });
        if data.logs.len() > LOG_LIMIT {
            data.logs.drain(..data.logs.len() - LOG_LIMIT);
        }
        let mut observations = Observations::default();
        for checkpoint in &data.cooldown_checkpoints {
            let id = checkpoint.last.account_id.clone();
            observations.last.insert(
                (id.clone(), checkpoint.last.model.clone()),
                checkpoint.last.clone(),
            );
            if let Some(t) = checkpoint.retry_at.filter(|t| *t > now()) {
                observations.retry.insert(id.clone(), t);
            }
            if let Some(t) = checkpoint.manual_retry_at.filter(|t| *t > now()) {
                observations.manual_retry.insert(id, t);
            }
        }
        for log in &data.logs {
            observations.last.insert(
                (log.result.account_id.clone(), log.result.model.clone()),
                log.result.clone(),
            );
            if let Some(retry_at) = log.retry_at.filter(|time| *time > now()) {
                if matches!(log.result.http_status, 429 | 401 | 403) {
                    observations
                        .manual_retry
                        .entry(log.result.account_id.clone())
                        .and_modify(|time| *time = (*time).max(retry_at))
                        .or_insert(retry_at);
                }
                observations
                    .retry
                    .entry(log.result.account_id.clone())
                    .and_modify(|time| *time = (*time).max(retry_at))
                    .or_insert(retry_at);
            }
        }
        Ok(Self {
            host,
            path,
            data: Arc::new(Mutex::new(data)),
            observations: Mutex::new(observations),
            persistence: Arc::new(AsyncMutex::new(())),
            request_slot: Semaphore::new(MAX_PROBE_CONCURRENCY),
            exit_slot: Semaphore::new(1),
        })
    }

    // 写成功后才发布内存状态；锁覆盖整个替换，防止旧快照覆盖新配置。
    async fn commit(
        &self,
        next: Persisted,
        guard: OwnedMutexGuard<()>,
    ) -> Result<(), ProviderAdminError> {
        let path = self.path.clone();
        let data = Arc::clone(&self.data);
        // fsync 不占用异步执行线程；锁由写盘任务持有，即使请求取消也保证写盘与发布有序。
        tokio::task::spawn_blocking(move || {
            let _guard = guard;
            let bytes =
                serde_json::to_vec(&next).map_err(|_| error(ProviderAdminErrorKind::Internal))?;
            atomic_write(&path, &bytes).map_err(|_| error(ProviderAdminErrorKind::Internal))?;
            *data
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = next;
            Ok(())
        })
        .await
        .map_err(|_| error(ProviderAdminErrorKind::Internal))?
    }

    pub async fn panel(&self) -> Result<TicketPanel, ProviderAdminError> {
        let accounts = self
            .host
            .accounts()
            .await
            .map_err(|_| error(ProviderAdminErrorKind::Unavailable))?;
        let bindings: BTreeMap<_, _> = accounts
            .iter()
            .filter_map(|a| a.binding.clone().map(|b| (a.id.clone(), b)))
            .collect();
        let d = self
            .data
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let o = self
            .observations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let rows = accounts
            .iter()
            .filter(|a| a.authentication_kind() == "oauth")
            .map(|a| {
                let id = a.id().as_str();
                let target = d.settings.target_length(id, a.plan_type());
                TicketAccountStatus {
                    id: id.into(),
                    name: a.name().into(),
                    plan: a.plan_type().map(str::to_owned),
                    eligible: eligible(a),
                    target_length: target,
                    policy: d.settings.accounts.get(id).cloned().unwrap_or_default(),
                    models: d
                        .settings
                        .models
                        .iter()
                        .map(|model| {
                            let ticket = d.tickets.iter().find(|t| {
                                t.account_id == id
                                    && eligible(a)
                                    && binding_matches(t, a, bindings.get(id).map(String::as_str))
                                    && t.model == *model
                                    && t.expires_at > now()
                                    && valid_state(&t.state, target)
                            });
                            let key = (id.to_owned(), model.clone());
                            TicketModelStatus {
                                model: model.clone(),
                                ready: ticket.is_some(),
                                expires_at: ticket.map(|t| t.expires_at),
                                token_issued_at: ticket.and_then(|t| token_timestamp(&t.state)),
                                last_requested_at: o.last_requests.get(&key).copied(),
                                auto_paused: d.settings.activity_only
                                    && o.last_requests.get(&key).is_none_or(|t| {
                                        t.saturating_add(d.settings.idle_seconds) <= now()
                                    }),
                                blocked: d.settings.enabled
                                    && d.settings.require_ticket
                                    && gated_model(model)
                                    && ticket.is_none(),
                                last_result: o.last.get(&key).cloned(),
                                busy: o.busy.contains_key(&key),
                                retry_at: o.retry_at(id, d.settings.interval_seconds, false),
                                manual_retry_at: o.retry_at(
                                    id,
                                    d.settings.manual_interval_seconds,
                                    true,
                                ),
                                continuous: o.continuous.get(&key).map(|run| {
                                    TicketContinuousStatus {
                                        interval_seconds: run.interval_seconds,
                                        next_probe_at: run.next_probe_at.max(
                                            o.retry_at(
                                                id,
                                                d.settings.manual_interval_seconds,
                                                true,
                                            )
                                            .unwrap_or(0),
                                        ),
                                        proxy_id: run
                                            .proxy
                                            .as_ref()
                                            .and_then(|proxy| {
                                                d.proxy_pool.iter().position(|p| p == proxy)
                                            })
                                            .map(|i| i.to_string()),
                                    }
                                }),
                            }
                        })
                        .collect(),
                }
            })
            .collect();
        let mut settings = d.settings.clone();
        settings.accounts.retain(|id, _| {
            accounts
                .iter()
                .any(|a| a.id().as_str() == id && a.authentication_kind() == "oauth")
        });
        Ok(TicketPanel {
            revision: d.revision,
            settings,
            proxy_configured: !d.proxy_pool.is_empty(),
            proxy_count: d.proxy_pool.len(),
            proxies: d
                .proxy_pool
                .iter()
                .enumerate()
                .map(|(index, proxy)| TicketProxyView {
                    exit_sample: o.exit_samples.get(proxy).cloned(),
                    enabled: d.proxy_enabled.get(index).copied().unwrap_or(true),
                    concurrency: d.proxy_concurrency.get(index).copied().unwrap_or(1),
                    in_flight: o
                        .proxy_busy
                        .get(&proxy_endpoint(proxy))
                        .copied()
                        .unwrap_or(0),
                    id: index.to_string(),
                    name: d
                        .proxy_names
                        .get(index)
                        .filter(|s| !s.is_empty())
                        .cloned()
                        .unwrap_or_else(|| format!("代理 {}", index + 1)),
                    endpoint: proxy_endpoint(proxy),
                    has_authentication: url::Url::parse(proxy)
                        .is_ok_and(|u| !u.username().is_empty() || u.password().is_some()),
                })
                .collect(),
            accounts: rows,
            logs: d.logs.iter().rev().cloned().collect(),
            log_limit: LOG_LIMIT,
            worker_checked_at: o.worker_checked_at,
        })
    }

    pub async fn update(&self, update: TicketUpdate) -> Result<TicketPanel, ProviderAdminError> {
        if !(10..=86400).contains(&update.settings.interval_seconds) {
            return Err(error(ProviderAdminErrorKind::Invalid).with_public_message(
                "自动探测间隔应为 10–86400 秒；手动立即重试请将手动间隔设为 0",
            ));
        }
        if update.settings.manual_interval_seconds > 86400 {
            return Err(error(ProviderAdminErrorKind::Invalid)
                .with_public_message("手动探测间隔应为 0–86400 秒"));
        }
        if !(60..=3600).contains(&update.settings.ttl_seconds)
            || update.settings.refresh_before_seconds >= update.settings.ttl_seconds
        {
            return Err(error(ProviderAdminErrorKind::Invalid)
                .with_public_message("有效期应为 60–3600 秒，提前刷新时间必须小于有效期"));
        }
        if [
            update.proxy_pool.is_some(),
            update.proxy_url.is_some(),
            update.proxies.is_some(),
        ]
        .into_iter()
        .filter(|v| *v)
        .count()
            > 1
        {
            return Err(error(ProviderAdminErrorKind::Invalid)
                .with_public_message("请只提交代理池或旧版单代理字段之一"));
        }
        let requested_pool = update
            .proxy_pool
            .as_ref()
            .map(|pool| {
                pool.iter()
                    .map(String::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(ToOwned::to_owned)
                    .collect::<Vec<_>>()
            })
            .or_else(|| {
                update
                    .proxy_url
                    .as_ref()
                    .map(|proxy| split_proxy_pool(proxy))
            });
        if !update.settings.validate() {
            return Err(error(ProviderAdminErrorKind::Invalid)
                .with_public_message("请检查目标长度、模型列表和账号策略；模型不能重复"));
        }
        if requested_pool
            .as_ref()
            .is_some_and(|pool| pool.len() > 64 || pool.iter().any(|item| !valid_proxy(item)))
        {
            return Err(error(ProviderAdminErrorKind::Invalid).with_public_message(
                "代理池最多 64 个，每行填写一个有效的 HTTP、HTTPS、SOCKS5 或 SOCKS5H 代理地址",
            ));
        }
        let accounts = self
            .host
            .accounts()
            .await
            .map_err(|_| error(ProviderAdminErrorKind::Unavailable))?;
        if update.settings.accounts.keys().any(|id| {
            !accounts
                .iter()
                .any(|a| a.id().as_str() == id && a.authentication_kind() == "oauth")
        }) {
            return Err(error(ProviderAdminErrorKind::Invalid)
                .with_public_message("账号列表已变化，请刷新页面后重新保存"));
        }
        let guard = Arc::clone(&self.persistence).lock_owned().await;
        let mut next = self
            .data
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        if next.revision != update.revision {
            return Err(error(ProviderAdminErrorKind::Conflict));
        }
        let previous = next.clone();
        let old_settings = next.settings.clone();
        next.settings = update.settings;
        if let Some(entries) = update.proxies {
            if entries.len() > 64 {
                return Err(error(ProviderAdminErrorKind::Invalid)
                    .with_public_message("代理池最多 64 个代理"));
            }
            let mut urls = Vec::new();
            let mut names = Vec::new();
            let mut enabled = Vec::new();
            let mut concurrency = Vec::new();
            for entry in entries {
                let previous_index = entry.id.as_deref().and_then(|id| id.parse::<usize>().ok());
                let limit = entry
                    .concurrency
                    .or_else(|| previous_index.and_then(|i| next.proxy_concurrency.get(i).copied()))
                    .unwrap_or(1);
                if !(1..=3).contains(&limit) {
                    return Err(error(ProviderAdminErrorKind::Invalid)
                        .with_public_message("每个代理入口并发应为 1–3"));
                }
                enabled.push(
                    entry
                        .enabled
                        .or_else(|| previous_index.and_then(|i| next.proxy_enabled.get(i).copied()))
                        .unwrap_or(true),
                );
                concurrency.push(limit);
                let name = entry.name.trim();
                if name.is_empty() || name.len() > 128 || name.chars().any(char::is_control) {
                    return Err(error(ProviderAdminErrorKind::Invalid)
                        .with_public_message("代理名称不能为空、不能包含控制字符且最多 128 字节"));
                }
                let imported = if let Some(id) = entry.saved_proxy_id.as_deref() {
                    if entry.id.is_some() || entry.url.is_some() {
                        return Err(error(ProviderAdminErrorKind::Invalid));
                    }
                    Some(self.host.proxy(id).await?)
                } else {
                    None
                };
                let url = match imported.or(entry.url).filter(|s| !s.trim().is_empty()) {
                    Some(url) => url.trim().to_owned(),
                    None => entry
                        .id
                        .as_deref()
                        .and_then(|id| id.parse::<usize>().ok())
                        .and_then(|i| next.proxy_pool.get(i))
                        .cloned()
                        .ok_or_else(|| {
                            error(ProviderAdminErrorKind::Invalid)
                                .with_public_message("新代理需要完整地址；已有代理请刷新后再保存")
                        })?,
                };
                if !valid_proxy(&url) {
                    return Err(error(ProviderAdminErrorKind::Invalid)
                        .with_public_message("代理地址格式不合法"));
                }
                urls.push(url);
                names.push(name.to_owned());
            }
            next.proxy_pool = urls;
            next.proxy_names = names;
            next.proxy_enabled = enabled;
            next.proxy_concurrency = concurrency;
            next.proxy_url = next.proxy_pool.join("\n");
        }
        if let Some(pool) = requested_pool {
            next.proxy_enabled = vec![true; pool.len()];
            next.proxy_concurrency = vec![1; pool.len()];
            next.proxy_names = (1..=pool.len()).map(|i| format!("代理 {i}")).collect();
            next.proxy_url = pool.join("\n");
            next.proxy_pool = pool;
        }
        if next.settings.enabled && next.proxy_pool.is_empty() {
            return Err(error(ProviderAdminErrorKind::Invalid)
                .with_public_message("启用打标需要先配置代理池；清除代理池前请关闭打标"));
        }
        // 无改动保存不落盘、不推进 revision，也不会使在途的有效结果失效。
        if next.settings == previous.settings
            && next.proxy_pool == previous.proxy_pool
            && next.proxy_names == previous.proxy_names
            && next.proxy_enabled == previous.proxy_enabled
            && next.proxy_concurrency == previous.proxy_concurrency
        {
            drop(guard);
            return self.panel().await;
        }
        next.revision = next
            .revision
            .checked_add(1)
            .ok_or_else(|| error(ProviderAdminErrorKind::Conflict))?;
        // 间隔、代理名称及手动/自动切换不改变已获票的身份和有效期。
        // 只淘汰被关闭、移除、目标长度不符或过期的票；缩短 TTL 只能收紧已有期限。
        next.tickets.retain_mut(|ticket| {
            let Some(account) = accounts
                .iter()
                .find(|a| a.id().as_str() == ticket.account_id)
            else {
                return false;
            };
            if next.settings.ttl_seconds < old_settings.ttl_seconds {
                ticket.expires_at = ticket
                    .expires_at
                    .saturating_sub(old_settings.ttl_seconds - next.settings.ttl_seconds);
            }
            next.settings.enabled
                && eligible(account)
                && next
                    .settings
                    .accounts
                    .get(&ticket.account_id)
                    .is_some_and(|p| p.mode != TicketMode::Off)
                && next.settings.models.contains(&ticket.model)
                && ticket.expires_at > now()
                && valid_state(
                    &ticket.state,
                    next.settings
                        .target_length(&ticket.account_id, account.plan_type()),
                )
        });
        self.commit(next, guard).await?;
        self.panel().await
    }

    /// 只记录真实业务调用；合成打标请求不经过此入口，不能自我唤醒。
    pub fn note_request(&self, account: &ProviderAccount, model: &str) -> bool {
        let d = self
            .data
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !d.settings.enabled || account.authentication_kind() != "oauth" {
            return false;
        }
        if d.settings.models.iter().any(|m| m == model)
            && d.settings.accounts.contains_key(account.id().as_str())
        {
            self.observations
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .last_requests
                .insert((account.id().as_str().to_owned(), model.to_owned()), now());
        }
        d.settings.require_ticket && gated_model(model)
    }

    pub fn get(&self, account: &ProviderAccount, model: &str) -> Option<String> {
        let binding = account.binding.clone();
        let d = self
            .data
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let id = account.id().as_str();
        if !eligible(account)
            || !d.settings.enabled
            || !d.settings.inject
            || !d.settings.models.iter().any(|m| m == model)
            || d.settings
                .accounts
                .get(id)
                .is_none_or(|p| p.mode == TicketMode::Off)
        {
            return None;
        }
        let target = d.settings.target_length(id, account.plan_type());
        d.tickets
            .iter()
            .find(|t| {
                t.account_id == id
                    && binding_matches(t, account, binding.as_deref())
                    && t.model == model
                    && t.expires_at > now()
                    && valid_state(&t.state, target)
            })
            .map(|t| t.state.clone())
    }

    pub async fn clear_logs(&self) -> Result<TicketPanel, ProviderAdminError> {
        let guard = Arc::clone(&self.persistence).lock_owned().await;
        let mut next = self
            .data
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        if next.logs.is_empty() {
            drop(guard);
            return self.panel().await;
        }
        // 清理可见历史不重置限流；每账号最多保留一个尚影响间隔/退避的检查点。
        let mut checkpoints = BTreeMap::<String, CooldownCheckpoint>::new();
        for checkpoint in &next.cooldown_checkpoints {
            checkpoints.insert(checkpoint.last.account_id.clone(), checkpoint.clone());
        }
        for log in &next.logs {
            let entry = checkpoints
                .entry(log.result.account_id.clone())
                .or_insert_with(|| CooldownCheckpoint {
                    last: log.result.clone(),
                    retry_at: None,
                    manual_retry_at: None,
                });
            if log.result.checked_at >= entry.last.checked_at {
                entry.last = log.result.clone();
            }
            entry.retry_at = entry.retry_at.max(log.retry_at);
            if matches!(log.result.http_status, 429 | 401 | 403) {
                entry.manual_retry_at = entry.manual_retry_at.max(log.retry_at);
            }
        }
        next.cooldown_checkpoints = checkpoints
            .into_values()
            .filter(|c| {
                c.last.checked_at.saturating_add(86400) > now()
                    || c.retry_at.is_some_and(|t| t > now())
            })
            .collect();
        next.logs.clear();
        self.commit(next, guard).await?;
        self.panel().await
    }

    /// 独立连接采样，不宣称是某次打标的实际出口；只手动触发，成功缓存十分钟。
    pub async fn sample_exit(
        &self,
        input: TicketExitProbe,
    ) -> Result<TicketExitSample, ProviderAdminError> {
        let proxy = {
            let d = self
                .data
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if d.revision != input.revision {
                return Err(error(ProviderAdminErrorKind::Conflict));
            }
            input
                .proxy_id
                .parse::<usize>()
                .ok()
                .and_then(|i| d.proxy_pool.get(i))
                .cloned()
                .ok_or_else(|| error(ProviderAdminErrorKind::Invalid))?
        };
        if let Some(sample) = self
            .observations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .exit_samples
            .get(&proxy)
            .filter(|s| {
                s.checked_at
                    .saturating_add(if s.ip.is_some() { EXIT_SAMPLE_TTL } else { 60 })
                    > now()
            })
            .cloned()
        {
            return Ok(sample);
        }
        let _slot = self.exit_slot.try_acquire().map_err(|_| {
            error(ProviderAdminErrorKind::Conflict)
                .with_public_message("已有出口检测正在进行，请稍后重试")
        })?;
        let measured = self.measure_exit(&proxy).await;
        let sample = match measured {
            Ok(ip) => TicketExitSample {
                ip: Some(ip),
                checked_at: now(),
                message: "独立连接采样，不保证与打标连接相同".into(),
            },
            Err(message) => TicketExitSample {
                ip: None,
                checked_at: now(),
                message: message.into(),
            },
        };
        let d = self
            .data
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if d.revision != input.revision {
            return Err(error(ProviderAdminErrorKind::Conflict)
                .with_public_message("代理配置已改变，请刷新后重新检测"));
        }
        let mut o = self
            .observations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        o.exit_samples.retain(|url, _| d.proxy_pool.contains(url));
        o.exit_samples.insert(proxy, sample.clone());
        Ok(sample)
    }

    async fn measure_exit(&self, proxy: &str) -> Result<std::net::IpAddr, &'static str> {
        self.host.exit(proxy).await
    }

    pub async fn continuous(
        &self,
        input: TicketContinuousInput,
    ) -> Result<TicketPanel, ProviderAdminError> {
        let key = (input.account_id.clone(), input.model.clone());
        let Some(interval) = input.interval_seconds else {
            if let Some(run) = self
                .observations
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .continuous
                .remove(&key)
            {
                run.cancellation.cancel();
            }
            return self.panel().await;
        };
        if !(10..=86400).contains(&interval) {
            return Err(error(ProviderAdminErrorKind::Invalid)
                .with_public_message("持续打标间隔应为 10–86400 秒"));
        }
        let panel = self.panel().await?;
        let account = panel
            .accounts
            .iter()
            .find(|a| a.id == input.account_id && a.eligible && a.policy.mode != TicketMode::Off)
            .ok_or_else(|| error(ProviderAdminErrorKind::Invalid))?;
        let model = account
            .models
            .iter()
            .find(|m| m.model == input.model)
            .ok_or_else(|| error(ProviderAdminErrorKind::Invalid))?;
        {
            let d = self
                .data
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if !d.settings.enabled
                || !d.settings.proxy_pool_enabled
                || !(0..d.proxy_pool.len()).any(|i| d.proxy_enabled.get(i).copied().unwrap_or(true))
            {
                return Err(error(ProviderAdminErrorKind::Invalid)
                    .with_public_message("请先启用打标及可用代理"));
            }
            if panel.revision != d.revision || input.revision.is_some_and(|r| r != d.revision) {
                return Err(error(ProviderAdminErrorKind::Conflict));
            }
            let proxy = if let Some(id) = input.proxy_id.as_deref() {
                if input.revision != Some(d.revision) {
                    return Err(error(ProviderAdminErrorKind::Conflict));
                }
                let i = id
                    .parse::<usize>()
                    .ok()
                    .filter(|i| {
                        *i < d.proxy_pool.len() && d.proxy_enabled.get(*i).copied().unwrap_or(true)
                    })
                    .ok_or_else(|| {
                        error(ProviderAdminErrorKind::Invalid).with_public_message("指定代理不可用")
                    })?;
                Some(d.proxy_pool[i].clone())
            } else {
                None
            };
            let mut o = self
                .observations
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if o.continuous.contains_key(&key) {
                return Err(error(ProviderAdminErrorKind::Conflict)
                    .with_public_message("持续打标已在运行，请先停止"));
            }
            if !model.ready {
                if o.continuous.len() >= MAX_PROBE_CONCURRENCY {
                    return Err(error(ProviderAdminErrorKind::Conflict));
                }
                o.continuous.insert(
                    key,
                    ContinuousProbe {
                        id: uuid::Uuid::new_v4(),
                        interval_seconds: interval,
                        next_probe_at: now(),
                        proxy,
                        cancellation: CancellationToken::new(),
                    },
                );
            }
        }
        self.panel().await
    }

    async fn run_continuous(&self, context: &CancellationToken) -> Result<(), ProviderAdminError> {
        let runs = self
            .observations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .continuous
            .clone();
        if runs.is_empty() {
            return Ok(());
        }
        let panel = self.panel().await?;
        let jobs = runs.into_iter().map(|(key, run)| {
            let panel = &panel;
            async move {
                let model = panel.accounts.iter().find(|a| a.id == key.0 && a.eligible && a.policy.mode != TicketMode::Off)
                    .and_then(|a| a.models.iter().find(|m| m.model == key.1));
                let (revision, proxy_index, proxy_available) = {
                    let d = self.data.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                    let index = run.proxy.as_ref().and_then(|proxy| d.proxy_pool.iter().position(|p| p == proxy));
                    let available = index.map_or_else(
                        || run.proxy.is_none() && (0..d.proxy_pool.len()).any(|i| d.proxy_enabled.get(i).copied().unwrap_or(true)),
                        |i| d.proxy_enabled.get(i).copied().unwrap_or(true));
                    (d.revision, index, available)
                };
                let stop = !panel.settings.enabled || !panel.settings.proxy_pool_enabled || !proxy_available || model.is_none_or(|m| m.ready);
                if !stop && (run.next_probe_at > now() || model.is_some_and(|m| m.busy || m.manual_retry_at.is_some_and(|t| t > now()))) { return; }
                let outcome = if stop { None } else {
                    tokio::select! {
                        () = context.cancelled() => return,
                        () = run.cancellation.cancelled() => return,
                        results = self.probe_continuous_batch(&key, revision, proxy_index) => Some(results),
                    }
                };
                let mut o = self.observations.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                if !o.continuous.get(&key).is_some_and(|active| active.id == run.id) { return; }
                let done = match &outcome {
                    None => true,
                    Some(results) => results.iter().any(|r| r.as_ref().is_ok_and(|r| r.matched))
                        || results.iter().any(|r| r.as_ref().is_err_and(|e| matches!(e.kind(), ProviderAdminErrorKind::Invalid | ProviderAdminErrorKind::Internal))),
                };
                if done {
                    if let Some(active) = o.continuous.remove(&key) { active.cancellation.cancel(); }
                } else if let Some(active) = o.continuous.get_mut(&key) {
                    let delay = if outcome.as_ref().is_some_and(|rs| rs.iter().any(|r| r.as_ref().is_ok_and(|r| r.http_status == 0))) { 60 } else { 0 };
                    active.next_probe_at = now().saturating_add(active.interval_seconds.max(delay));
                }
            }
        });
        futures::future::join_all(jobs).await;
        Ok(())
    }

    async fn probe_continuous_batch(
        &self,
        key: &(String, String),
        revision: u64,
        selected: Option<usize>,
    ) -> Vec<Result<TicketResult, ProviderAdminError>> {
        let slots = {
            let d = self
                .data
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let o = self
                .observations
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if o.busy.contains_key(key)
                || o.retry_at(&key.0, d.settings.manual_interval_seconds, true)
                    .is_some()
            {
                return vec![Err(error(ProviderAdminErrorKind::Conflict))];
            }
            let mut slots = Vec::new();
            let mut endpoints = std::collections::BTreeSet::new();
            for offset in 0..d.proxy_pool.len() {
                let i = (o.proxy_cursor + offset) % d.proxy_pool.len();
                if selected.is_some_and(|s| s != i)
                    || !d.proxy_enabled.get(i).copied().unwrap_or(true)
                {
                    continue;
                }
                let endpoint = proxy_endpoint(&d.proxy_pool[i]);
                if !endpoints.insert(endpoint.clone()) {
                    continue;
                }
                let limit = d
                    .proxy_pool
                    .iter()
                    .enumerate()
                    .filter(|(j, p)| {
                        d.proxy_enabled.get(*j).copied().unwrap_or(true)
                            && proxy_endpoint(p) == endpoint
                    })
                    .map(|(j, _)| d.proxy_concurrency.get(j).copied().unwrap_or(1))
                    .min()
                    .unwrap_or(1);
                let available =
                    limit.saturating_sub(o.proxy_busy.get(&endpoint).copied().unwrap_or(0));
                for _ in 0..available {
                    if slots.len() < MAX_PROBE_CONCURRENCY {
                        slots.push(i);
                    }
                }
            }
            slots
        };
        futures::future::join_all(slots.into_iter().map(|i| {
            self.probe_on_proxy(
                TicketProbe {
                    account_id: key.0.clone(),
                    model: key.1.clone(),
                    proxy_id: Some(i.to_string()),
                    revision: Some(revision),
                },
                false,
                Some(i),
                true,
            )
        }))
        .await
    }

    pub async fn probe(&self, input: TicketProbe) -> Result<TicketResult, ProviderAdminError> {
        self.probe_with_mode(input, false).await
    }

    async fn probe_with_mode(
        &self,
        input: TicketProbe,
        automatic: bool,
    ) -> Result<TicketResult, ProviderAdminError> {
        self.probe_on_proxy(input, automatic, None, false).await
    }

    async fn probe_on_proxy(
        &self,
        input: TicketProbe,
        automatic: bool,
        forced_proxy: Option<usize>,
        continuous: bool,
    ) -> Result<TicketResult, ProviderAdminError> {
        let _permit = self
            .request_slot
            .try_acquire()
            .map_err(|_| error(ProviderAdminErrorKind::Conflict))?;
        let account_id = input.account_id.clone();
        let account = self
            .host
            .account(&account_id)
            .await?
            .filter(eligible)
            .ok_or_else(|| error(ProviderAdminErrorKind::Invalid))?;
        let snapshot = self
            .data
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        if input.revision.is_some_and(|r| r != snapshot.revision) {
            return Err(error(ProviderAdminErrorKind::Conflict)
                .with_public_message("代理或策略已变化，请刷新后重试"));
        }
        let forced_proxy = if let Some(id) = input.proxy_id.as_deref() {
            if input.revision != Some(snapshot.revision) {
                return Err(error(ProviderAdminErrorKind::Conflict)
                    .with_public_message("指定代理需要当前策略版本，请刷新后重试"));
            }
            Some(
                id.parse::<usize>()
                    .ok()
                    .filter(|i| *i < snapshot.proxy_pool.len())
                    .ok_or_else(|| error(ProviderAdminErrorKind::Invalid))?,
            )
        } else {
            forced_proxy
        };
        if !snapshot.settings.enabled
            || !snapshot.settings.proxy_pool_enabled
            || snapshot.proxy_pool.is_empty()
            || !snapshot.settings.models.contains(&input.model)
            || snapshot
                .settings
                .accounts
                .get(&input.account_id)
                .is_none_or(|p| p.mode == TicketMode::Off)
            || (automatic
                && snapshot
                    .settings
                    .accounts
                    .get(&input.account_id)
                    .is_none_or(|p| p.mode != TicketMode::Auto))
        {
            return Err(error(ProviderAdminErrorKind::Invalid));
        }
        let (proxy, proxy_name, proxy_key, exit_sample) = {
            let mut o = self
                .observations
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let interval = if automatic {
                snapshot.settings.interval_seconds
            } else {
                snapshot.settings.manual_interval_seconds
            };
            // 持续批次在派发前统一检查冷却，不因同批先完成的一发阻止其余并发槽。
            if !continuous
                && o.retry_at(&input.account_id, interval, !automatic)
                    .is_some()
            {
                return Err(error(ProviderAdminErrorKind::Conflict)
                    .with_public_message("账号仍在探测间隔或上游错误退避中，请稍后重试"));
            }
            let key = (input.account_id.clone(), input.model.clone());
            if !automatic && !continuous && o.busy.contains_key(&key) {
                return Err(error(ProviderAdminErrorKind::Conflict)
                    .with_public_message("该账号模型已有探测正在执行"));
            }
            let start = o.proxy_cursor;
            let index = (0..snapshot.proxy_pool.len())
                .map(|offset| (start + offset) % snapshot.proxy_pool.len())
                .find(|index| {
                    if forced_proxy.is_some_and(|forced| forced != *index)
                        || !snapshot.proxy_enabled.get(*index).copied().unwrap_or(true)
                    {
                        return false;
                    }
                    let endpoint = proxy_endpoint(&snapshot.proxy_pool[*index]);
                    // 重复入口共用限额，不因重命名、认证别名或列表重排而绕过并发约束。
                    let limit = snapshot
                        .proxy_pool
                        .iter()
                        .enumerate()
                        .filter(|(i, p)| {
                            snapshot.proxy_enabled.get(*i).copied().unwrap_or(true)
                                && proxy_endpoint(p) == endpoint
                        })
                        .map(|(i, _)| snapshot.proxy_concurrency.get(i).copied().unwrap_or(1))
                        .min()
                        .unwrap_or(1);
                    o.proxy_busy.get(&endpoint).copied().unwrap_or(0) < limit
                })
                .ok_or_else(|| {
                    error(ProviderAdminErrorKind::Conflict)
                        .with_public_message("代理池已暂停、全部代理已禁用或并发已满")
                })?;
            o.proxy_cursor = index.wrapping_add(1);
            let proxy = snapshot.proxy_pool[index].clone();
            let endpoint = proxy_endpoint(&proxy);
            let exit_sample = o
                .exit_samples
                .get(&proxy)
                .filter(|s| s.ip.is_some() && s.checked_at.saturating_add(EXIT_SAMPLE_TTL) > now())
                .cloned();
            *o.proxy_busy.entry(endpoint.clone()).or_default() += 1;
            *o.busy.entry(key).or_default() += 1;
            (
                proxy,
                snapshot
                    .proxy_names
                    .get(index)
                    .cloned()
                    .unwrap_or_else(|| format!("代理 {}", index + 1)),
                endpoint,
                exit_sample,
            )
        };
        let _busy = BusyGuard {
            observations: &self.observations,
            key: (input.account_id.clone(), input.model.clone()),
            proxy: proxy_key,
        };
        let target = snapshot
            .settings
            .target_length(&input.account_id, account.plan_type());
        let started_at = now();
        let started = Instant::now();
        let binding = account.binding.clone();
        let outcome = self.host.probe(&account, &input.model, &proxy).await;
        let (status, state, retry, message) =
            outcome.unwrap_or_else(|message| (0, String::new(), 60, message.into()));
        let current = self.host.account(&account_id).await;
        let account_unchanged = current.ok().flatten().is_some_and(|a| {
            eligible(&a) && a.plan == account.plan && a.binding == binding && binding.is_some()
        });
        let mut matched = status == 200 && valid_state(&state, target) && account_unchanged;
        let guard = Arc::clone(&self.persistence).lock_owned().await;
        let mut next = self
            .data
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        if next.revision != snapshot.revision {
            matched = false;
        }
        let result = TicketResult {
            account_id: input.account_id.clone(),
            model: input.model.clone(),
            http_status: status,
            length: state.len(),
            matched,
            checked_at: now(),
            message: if next.revision != snapshot.revision || !account_unchanged {
                "策略或账号已改变，或账号状态无法复核，结果未采纳".into()
            } else if matched {
                "已匹配目标规则".into()
            } else {
                message
            },
        };
        let token_issued_at = token_timestamp(&state);
        if matched {
            next.tickets.retain(|t| {
                t.expires_at > now()
                    && !(t.account_id == input.account_id && t.model == input.model)
            });
            next.tickets.push(Ticket {
                account_id: input.account_id.clone(),
                credential_revision: account.revision().get(),
                auth_binding: binding,
                model: input.model.clone(),
                state,
                expires_at: now() + snapshot.settings.ttl_seconds,
            });
        }
        let delay = match status {
            429 => retry.max(60),
            401 | 403 => 900,
            0 => 60,
            _ => 0,
        };
        let retry_at = result.checked_at.saturating_add(delay);
        next.logs.push(TicketLog {
            token_issued_at,
            exit_sample,
            id: uuid::Uuid::new_v4().to_string(),
            account_name: account.name().into(),
            trigger: if automatic {
                TicketMode::Auto
            } else {
                TicketMode::Manual
            },
            continuous,
            proxy_endpoint: proxy_endpoint(&proxy),
            proxy_name,
            target_length: target,
            started_at,
            duration_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
            retry_at: (delay > 0).then_some(retry_at),
            result: result.clone(),
        });
        if next.logs.len() > LOG_LIMIT {
            next.logs.drain(..next.logs.len() - LOG_LIMIT);
        }
        // 失败也落盘；不保存票原文、代理认证或上游正文。
        let committed = self.commit(next, guard).await;
        let mut o = self
            .observations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        o.retry
            .entry(input.account_id.clone())
            .and_modify(|t| *t = (*t).max(retry_at))
            .or_insert(retry_at);
        if matches!(status, 429 | 401 | 403) {
            o.manual_retry
                .entry(input.account_id.clone())
                .and_modify(|t| *t = (*t).max(retry_at))
                .or_insert(retry_at);
        }
        o.last
            .insert((input.account_id, input.model), result.clone());
        committed.map_err(|_| {
            error(ProviderAdminErrorKind::Internal)
                .with_public_message("探测已执行，但日志或票保存失败，请检查磁盘状态")
        })?;
        Ok(result)
    }
}

fn proxy_endpoint(raw: &str) -> String {
    let Ok(url) = url::Url::parse(raw) else {
        return "未知代理".into();
    };
    // 只组合白名单字段，不在 URL 原文上做字符串脱敏。
    let host = url
        .host()
        .map_or_else(|| "未知主机".into(), |host| host.to_string());
    let port = url
        .port_or_known_default()
        .or_else(|| matches!(url.scheme(), "socks5" | "socks5h").then_some(1080));
    match port {
        Some(port) => format!("{}://{host}:{port}", url.scheme()),
        None => format!("{}://{host}", url.scheme()),
    }
}

struct BusyGuard<'a> {
    observations: &'a Mutex<Observations>,
    key: (String, String),
    proxy: String,
}
impl Drop for BusyGuard<'_> {
    fn drop(&mut self) {
        let mut observations = self
            .observations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(count) = observations.busy.get_mut(&self.key) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                observations.busy.remove(&self.key);
            }
        }
        if let Some(count) = observations.proxy_busy.get_mut(&self.proxy) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                observations.proxy_busy.remove(&self.proxy);
            }
        }
    }
}

fn valid_proxy(raw: &str) -> bool {
    if raw.is_empty() {
        return true;
    }
    if raw.len() > 4096 {
        return false;
    }
    url::Url::parse(raw).is_ok_and(|u| {
        matches!(u.scheme(), "http" | "https" | "socks5" | "socks5h")
            && u.host_str().is_some()
            && u.query().is_none()
            && u.fragment().is_none()
            && matches!(u.path(), "" | "/")
    })
}

fn split_proxy_pool(raw: &str) -> Vec<String> {
    raw.lines()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

fn atomic_write(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let parent = path
        .parent()
        .ok_or_else(|| std::io::Error::other("missing parent"))?;
    std::fs::create_dir_all(parent)?;
    let tmp = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
    let mut options = std::fs::OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let result = (|| {
        let mut file = options.open(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        std::fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(tmp);
    }
    result
}

pub struct TicketTask(pub Arc<TicketService>);
impl TicketTask {
    pub fn run_cycle(
        &self,
        context: CancellationToken,
    ) -> BoxFuture<'_, Result<(), ProviderAdminError>> {
        Box::pin(async move {
            self.0
                .observations
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .worker_checked_at = Some(now());
            self.0
                .run_continuous(&context)
                .await
                .map_err(|_| error(ProviderAdminErrorKind::Unavailable))?;
            // 仅手动且没有持续任务时不访问账号数据库；心跳仍可供页面判断 worker 是否存活。
            let automatic_enabled = {
                let d = self
                    .0
                    .data
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                d.settings.enabled
                    && d.settings.proxy_pool_enabled
                    && !d.proxy_pool.is_empty()
                    && d.settings
                        .accounts
                        .values()
                        .any(|p| p.mode == TicketMode::Auto)
            };
            if !automatic_enabled {
                return Ok(());
            }
            let panel = self
                .0
                .panel()
                .await
                .map_err(|_| error(ProviderAdminErrorKind::Unavailable))?;
            if !panel.settings.enabled
                || !panel.settings.proxy_pool_enabled
                || !panel.proxy_configured
            {
                return Ok(());
            }
            let candidates: Vec<_> = panel
                .accounts
                .iter()
                .filter(|a| a.eligible && a.policy.mode == TicketMode::Auto)
                .flat_map(|a| {
                    a.models
                        .iter()
                        .filter(|m| {
                            !m.busy
                                && !m.auto_paused
                                && m.continuous.is_none()
                                && m.retry_at.is_none()
                                && m.expires_at.is_none_or(|t| {
                                    t <= now() + panel.settings.refresh_before_seconds
                                })
                        })
                        .map(|m| TicketProbe {
                            account_id: a.id.clone(),
                            model: m.model.clone(),
                            ..Default::default()
                        })
                })
                .collect();
            if candidates.is_empty() {
                return Ok(());
            }
            let input = {
                let o = self
                    .0
                    .observations
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                candidates
                    .into_iter()
                    .min_by_key(|p| {
                        o.last
                            .get(&(p.account_id.clone(), p.model.clone()))
                            .map_or(0, |r| r.checked_at)
                    })
                    .expect("候选非空")
            };
            // 同一账号模型按各入口配额并发尝试；达到全局上限后从轮换游标公平选择。
            let snapshot = self
                .0
                .data
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone();
            let start = self
                .0
                .observations
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .proxy_cursor;
            let mut slots = Vec::new();
            for offset in 0..snapshot.proxy_pool.len() {
                let index = (start + offset) % snapshot.proxy_pool.len();
                if snapshot.proxy_enabled.get(index).copied().unwrap_or(true) {
                    for _ in 0..snapshot.proxy_concurrency.get(index).copied().unwrap_or(1) {
                        if slots.len() < MAX_PROBE_CONCURRENCY {
                            slots.push(index);
                        }
                    }
                }
            }
            let probes = futures::future::join_all(slots.into_iter().map(|index| {
                self.0.probe_on_proxy(
                    TicketProbe {
                        account_id: input.account_id.clone(),
                        model: input.model.clone(),
                        ..Default::default()
                    },
                    true,
                    Some(index),
                    false,
                )
            }));
            tokio::select! {
                () = context.cancelled() => {},
                results = probes => {
                    for result in results {
                        match result {
                            Ok(result) => tracing::info!(account_id = %result.account_id, model = %result.model,
                                http = result.http_status, length = result.length, matched = result.matched, "后台打标完成"),
                            Err(error) => tracing::warn!(kind = ?error.kind(), "后台打标未执行"),
                        }
                    }
                }
            }
            Ok(())
        })
    }
}
