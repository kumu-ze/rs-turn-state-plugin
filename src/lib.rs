pub mod settings;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use settings::{TicketMode, TicketSettings};
use std::{
    collections::BTreeMap,
    fs,
    io::{Read, Write},
    path::Path,
};

pub const MAX_FRAME: usize = 1024 * 1024;

#[derive(Clone, Deserialize, Serialize)]
pub struct Ticket {
    pub account_id: String,
    #[serde(default)]
    pub auth_binding: Option<String>,
    pub model: String,
    pub state: String,
    pub expires_at: u64,
}

#[derive(Deserialize, Serialize)]
pub struct Store {
    pub revision: u64,
    pub settings: TicketSettings,
    pub tickets: Vec<Ticket>,
    // 原始代理、日志和退避字段完整保留，供后续探测迁移使用，不向页面返回。
    #[serde(flatten)]
    pub retained: BTreeMap<String, Value>,
}

impl Default for Store {
    fn default() -> Self {
        Self {
            revision: 1,
            settings: TicketSettings::default(),
            tickets: Vec::new(),
            retained: BTreeMap::new(),
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestContext {
    pub provider: String,
    pub account_id: String,
    pub model: String,
    pub credential_scope: String,
    pub authentication_kind: String,
    pub plan_type: Option<String>,
    pub account_eligible: bool,
}

#[derive(Default, Serialize)]
pub struct Decision {
    pub deny: bool,
    pub values: BTreeMap<String, String>,
}

fn valid_state(state: &str, length: usize) -> bool {
    state.len() == length
        && state.starts_with("gAAAAA")
        && state
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"_-=".contains(&c))
}

fn gated_model(model: &str) -> bool {
    let model = model.to_ascii_lowercase();
    ["gpt-6", "gpt-5.6"].iter().any(|prefix| {
        model
            .strip_prefix(prefix)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with('-') || rest.starts_with('.'))
    })
}

impl Store {
    pub fn load(path: &Path, now: u64) -> Result<Self, &'static str> {
        let file = fs::File::open(path).map_err(|_| "无法读取插件数据")?;
        let mut bytes = Vec::new();
        file.take(16 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| "无法读取插件数据")?;
        if bytes.len() > 16 * 1024 * 1024 {
            return Err("插件数据过大");
        }
        let mut store: Self = serde_json::from_slice(&bytes).map_err(|_| "插件数据格式无效")?;
        if !store.settings.validate() {
            return Err("插件策略无效");
        }
        // 无身份摘要的历史票不能安全跨宿主迁移，保留设置但不复用这类票。
        store.tickets.retain(|t| {
            t.auth_binding
                .as_ref()
                .is_some_and(|b| b.len() == 64 && b.bytes().all(|c| c.is_ascii_hexdigit()))
                && t.expires_at > now
                && t.expires_at <= now.saturating_add(3600)
                && (64..=4096).contains(&t.state.len())
                && valid_state(&t.state, t.state.len())
        });
        Ok(store)
    }

    pub fn decision(&self, context: &RequestContext, now: u64) -> Decision {
        let mut result = Decision::default();
        if context.provider != "openai"
            || context.authentication_kind != "oauth"
            || !self.settings.enabled
        {
            return result;
        }
        let target = self
            .settings
            .target_length(&context.account_id, context.plan_type.as_deref());
        let ticket = if context.account_eligible
            && self.settings.inject
            && self.settings.models.contains(&context.model)
            && self
                .settings
                .accounts
                .get(&context.account_id)
                .is_some_and(|p| p.mode != TicketMode::Off)
        {
            self.tickets.iter().find(|t| {
                t.account_id == context.account_id
                    && t.model == context.model
                    && t.auth_binding.as_deref() == Some(context.credential_scope.as_str())
                    && t.expires_at > now
                    && valid_state(&t.state, target)
            })
        } else {
            None
        };
        if let Some(ticket) = ticket {
            result
                .values
                .insert("session_state".into(), ticket.state.clone());
        }
        result.deny =
            self.settings.require_ticket && gated_model(&context.model) && ticket.is_none();
        result
    }

    pub fn panel(&self, now: u64) -> Value {
        json!({"revision":self.revision, "settings":self.settings, "probeSupported":false,
            "tickets":self.tickets.iter().map(|t| json!({"accountId":t.account_id,"model":t.model,
                "expiresAt":t.expires_at,"expired":t.expires_at <= now})).collect::<Vec<_>>()})
    }
}

/// 显式离线导入；目标存在时拒绝覆盖，源文件不变。失败时删除未完成的新文件。
pub fn import(source: &Path, destination: &Path, now: u64) -> Result<usize, &'static str> {
    let store = Store::load(source, now)?;
    let bytes = serde_json::to_vec(&store).map_err(|_| "无法编码插件数据")?;
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(destination)
        .map_err(|_| "目标已存在或不可写")?;
    if file
        .write_all(&bytes)
        .and_then(|_| file.sync_all())
        .is_err()
    {
        drop(file);
        let _ = fs::remove_file(destination);
        return Err("导入写盘失败");
    }
    Ok(store.tickets.len())
}

pub fn read_frame(input: &mut impl Read) -> Result<Option<Value>, &'static str> {
    let mut prefix = [0; 4];
    match input.read(&mut prefix[..1]) {
        Ok(0) => return Ok(None),
        Ok(_) => {}
        Err(_) => return Err("读取失败"),
    }
    input.read_exact(&mut prefix[1..]).map_err(|_| "帧不完整")?;
    let length = u32::from_be_bytes(prefix) as usize;
    if length == 0 || length > MAX_FRAME {
        return Err("帧大小无效");
    }
    let mut body = vec![0; length];
    input.read_exact(&mut body).map_err(|_| "帧不完整")?;
    serde_json::from_slice(&body)
        .map(Some)
        .map_err(|_| "帧格式无效")
}

pub fn write_frame(output: &mut impl Write, value: &Value) -> Result<(), &'static str> {
    let body = serde_json::to_vec(value).map_err(|_| "编码失败")?;
    if body.len() > MAX_FRAME {
        return Err("响应过大");
    }
    output
        .write_all(&(body.len() as u32).to_be_bytes())
        .and_then(|_| output.write_all(&body))
        .and_then(|_| output.flush())
        .map_err(|_| "写入失败")
}
