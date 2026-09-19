use crate::{
    RequestContext,
    bridge::{Account, ErrorKind, HostBridge, ServiceError},
    tickets::{TicketService, TicketTask},
};
use serde_json::{Value, json};
use std::{path::Path, sync::Arc};
use tokio_util::sync::CancellationToken;

pub struct Runtime {
    service: Arc<TicketService>,
    host: HostBridge,
    cancellation: CancellationToken,
    worker: tokio::task::JoinHandle<()>,
    jobs: std::sync::Mutex<
        std::collections::BTreeMap<String, tokio::task::JoinHandle<Result<Value, ServiceError>>>,
    >,
}
impl Runtime {
    pub fn new(path: &Path) -> Result<Self, ServiceError> {
        let host = HostBridge::new()?;
        let service = Arc::new(TicketService::new(host.clone(), path.to_path_buf())?);
        let cancellation = CancellationToken::new();
        let cancel = cancellation.clone();
        let task = TicketTask(service.clone());
        let worker = tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(1));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tokio::select! {_=cancel.cancelled()=>break,_=interval.tick()=>{let _=task.run_cycle(cancel.clone()).await;}}
            }
        });
        Ok(Self {
            service,
            host,
            cancellation,
            worker,
            jobs: Default::default(),
        })
    }
    pub async fn invoke(&self, method: &str, input: Value) -> Result<Value, ServiceError> {
        let invalid = || ServiceError::new(ErrorKind::Invalid);

        match method {
            "request.before_send" => {
                let request: RequestContext =
                    serde_json::from_value(input).map_err(|_| invalid())?;
                if request.provider != "openai" {
                    return Ok(json!({"deny":false,"values":{}}));
                }
                let account = Account {
                    id: request.account_id,
                    name: String::new(),
                    plan: request.plan_type,
                    eligible: request.account_eligible,
                    binding: Some(request.credential_scope),
                    revision: 0,
                    authentication_kind: request.authentication_kind,
                };
                let gated = self.service.note_request(&account, &request.model);
                let ticket = self.service.get(&account, &request.model);
                Ok(
                    json!({"deny":gated&&ticket.is_none(),"values":ticket.map_or_else(||json!({}),|state|json!({"session_state":state}))}),
                )
            }
            "admin.panel" => encode(self.service.panel().await?),
            "admin.update" => encode(
                self.service
                    .update(serde_json::from_value(input).map_err(|_| invalid())?)
                    .await?,
            ),
            "admin.continuous" => encode(
                self.service
                    .continuous(serde_json::from_value(input).map_err(|_| invalid())?)
                    .await?,
            ),
            "admin.clear_logs" => encode(self.service.clear_logs().await?),
            "admin.probe" => {
                let probe = serde_json::from_value(input).map_err(|_| invalid())?;
                let service = self.service.clone();
                self.start_job(async move { encode(service.probe(probe).await?) })
            }
            "admin.exit" => {
                let probe = serde_json::from_value(input).map_err(|_| invalid())?;
                let service = self.service.clone();
                self.start_job(async move { encode(service.sample_exit(probe).await?) })
            }
            "admin.job" => {
                let id = input["id"].as_str().ok_or_else(invalid)?;
                let task = {
                    let mut jobs = self.jobs.lock().unwrap();
                    let task = jobs.get(id).ok_or_else(invalid)?;
                    if !task.is_finished() {
                        return Ok(json!({"pending":true}));
                    }
                    jobs.remove(id).unwrap()
                };
                match task.await {
                    Ok(Ok(result)) => Ok(json!({"result":result})),
                    Ok(Err(error)) => Ok(json!({"error":error.message()})),
                    Err(_) => Ok(json!({"error":"探测任务失败"})),
                }
            }
            "admin.proxies" => self.host.invoke("proxies.list", input).await,
            "admin.ui" => Ok(json!({"html":include_str!("page.html")})),
            _ => Err(invalid()),
        }
    }
    fn start_job(
        &self,
        future: impl std::future::Future<Output = Result<Value, ServiceError>> + Send + 'static,
    ) -> Result<Value, ServiceError> {
        let mut jobs = self.jobs.lock().unwrap();
        if jobs.len() >= 32 {
            jobs.retain(|_, task| !task.is_finished());
            if jobs.len() >= 32 {
                return Err(ServiceError::new(ErrorKind::Conflict));
            }
        }
        let id = uuid::Uuid::new_v4().to_string();
        jobs.insert(id.clone(), tokio::spawn(future));
        Ok(json!({"jobId":id}))
    }
}
impl Drop for Runtime {
    fn drop(&mut self) {
        self.cancellation.cancel();
        self.worker.abort();
        for task in self.jobs.get_mut().unwrap().values() {
            task.abort();
        }
    }
}

fn encode<T: serde::Serialize>(value: T) -> Result<Value, ServiceError> {
    serde_json::to_value(value).map_err(|_| ServiceError::new(ErrorKind::Internal))
}
