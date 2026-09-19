use rs_turn_state_plugin::{import, read_frame, write_frame};
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn run() -> Result<(), &'static str> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() == 3 && args[0] == "import" {
        let count = import(&PathBuf::from(&args[1]), &PathBuf::from(&args[2]), now())?;
        eprintln!("已导入 {count} 张具有身份摘要的未到期票；源文件保留。");
        return Ok(());
    }
    if !args.is_empty() {
        return Err("用法：rs-turn-state-plugin [import <旧文件> <新文件>]");
    }
    let root = std::env::var_os("RS_PLUGIN_DATA_DIR").ok_or("缺少插件数据目录")?;
    let path = PathBuf::from(root).join("turn-state-tickets.json");
    let executor = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|_| "无法启动运行时")?;
    let _entered = executor.enter();
    let runtime =
        rs_turn_state_plugin::runtime::Runtime::new(&path).map_err(|_| "插件初始化失败")?;
    let mut initialized = false;
    let mut input = std::io::stdin().lock();
    let mut output = std::io::stdout().lock();
    while let Some(request) = read_frame(&mut input)? {
        if request["jsonrpc"] != "2.0" || !request["id"].is_u64() {
            return Err("协议请求无效");
        }
        let result: Result<Value, &str> = match request["method"].as_str() {
            Some("initialize")
                if !initialized
                    && request["params"] == json!({"apiVersion":1,"pluginId":"turn-state"}) =>
            {
                initialized = true;
                Ok(request["params"].clone())
            }
            _ if !initialized => Err("需要先握手"),
            Some(method) => executor
                .block_on(runtime.invoke(method, request["params"].clone()))
                .map_err(|e| e.message()),
            None => Err("缺少方法"),
        };
        let response = match result {
            Ok(result) => json!({"jsonrpc":"2.0","id":request["id"],"result":result}),
            Err(message) => {
                json!({"jsonrpc":"2.0","id":request["id"],"error":{"code":-32601,"message":message}})
            }
        };
        write_frame(&mut output, &response)?;
    }
    Ok(())
}

fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
