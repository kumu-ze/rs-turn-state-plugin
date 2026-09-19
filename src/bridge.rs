use serde::{Deserialize, de::DeserializeOwned};
use serde_json::{Value, json};

#[derive(Debug, Clone, Copy)]
pub enum ErrorKind {
    Invalid,
    Conflict,
    Unavailable,
    Internal,
}
#[derive(Debug)]
pub struct ServiceError {
    kind: ErrorKind,
    message: Option<&'static str>,
}
impl ServiceError {
    pub fn new(kind: ErrorKind) -> Self {
        Self {
            kind,
            message: None,
        }
    }
    pub fn kind(&self) -> ErrorKind {
        self.kind
    }
    pub fn with_public_message(mut self, message: &'static str) -> Self {
        self.message = Some(message);
        self
    }
    pub fn message(&self) -> &'static str {
        self.message.unwrap_or("插件操作失败，请刷新后重试")
    }
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Account {
    pub id: String,
    pub name: String,
    pub plan: Option<String>,
    pub eligible: bool,
    pub binding: Option<String>,
    pub revision: u64,
    pub authentication_kind: String,
}
impl Account {
    pub fn id(&self) -> &String {
        &self.id
    }
    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn plan_type(&self) -> Option<&str> {
        self.plan.as_deref()
    }
    pub fn authentication_kind(&self) -> &str {
        &self.authentication_kind
    }
    pub fn revision(&self) -> Revision {
        Revision(self.revision)
    }
}
pub struct Revision(u64);
impl Revision {
    pub fn get(&self) -> u64 {
        self.0
    }
}

#[derive(Clone)]
pub struct HostBridge {
    url: String,
    token: String,
    client: reqwest::Client,
}
impl HostBridge {
    pub fn new() -> Result<Self, ServiceError> {
        let fail = || ServiceError::new(ErrorKind::Unavailable);
        let url = std::env::var("RS_PLUGIN_SERVICES_URL").map_err(|_| fail())?;
        let parsed = url::Url::parse(&url).map_err(|_| fail())?;
        if parsed.scheme() != "http" || parsed.host_str() != Some("127.0.0.1") {
            return Err(fail());
        }
        Ok(Self {
            url,
            token: std::env::var("RS_PLUGIN_SERVICES_TOKEN").map_err(|_| fail())?,
            client: reqwest::Client::builder()
                .no_proxy()
                .timeout(std::time::Duration::from_secs(31))
                .build()
                .map_err(|_| fail())?,
        })
    }
    pub async fn invoke<T: DeserializeOwned>(
        &self,
        method: &str,
        input: Value,
    ) -> Result<T, ServiceError> {
        self.client
            .post(&self.url)
            .header("authorization", &self.token)
            .json(&json!({"method":method,"input":input}))
            .send()
            .await
            .map_err(|_| ServiceError::new(ErrorKind::Unavailable))?
            .error_for_status()
            .map_err(|_| ServiceError::new(ErrorKind::Unavailable))?
            .json()
            .await
            .map_err(|_| ServiceError::new(ErrorKind::Unavailable))
    }
    pub async fn accounts(&self) -> Result<Vec<Account>, ServiceError> {
        self.invoke("accounts.list", json!({})).await
    }
    pub async fn account(&self, id: &str) -> Result<Option<Account>, ServiceError> {
        self.invoke("accounts.get", json!({"accountId":id})).await
    }
    pub async fn proxy(&self, id: &str) -> Result<String, ServiceError> {
        self.invoke("proxies.resolve", json!({"id":id})).await
    }
    pub async fn probe(
        &self,
        account: &Account,
        model: &str,
        proxy: &str,
    ) -> Result<(u16, String, u64, String), &'static str> {
        self.invoke("responses.probe",json!({"accountId":account.id,"credentialScope":account.binding,"model":model,"proxy":proxy})).await.map_err(|_|"宿主探测失败")
    }
    pub async fn exit(&self, proxy: &str) -> Result<std::net::IpAddr, &'static str> {
        self.invoke("network.exit", json!({"proxy":proxy}))
            .await
            .map_err(|_| "出口检测失败")
    }
}
