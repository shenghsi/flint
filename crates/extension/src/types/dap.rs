use collections::HashMap;
use gpui::SharedString;
use serde::Serialize;
use std::{net::IpAddr, path::PathBuf};

pub use dap_types::{StartDebuggingRequestArguments, StartDebuggingRequestArgumentsRequest};
pub use task::{
    AttachRequest, BuildTaskDefinition, DebugRequest, DebugScenario, LaunchRequest,
    TaskTemplate as BuildTaskTemplate, TcpArgumentsTemplate,
};

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TcpArguments {
    pub host: IpAddr,
    pub port: u16,
    pub timeout: Option<u64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DebugTaskDefinition {
    pub label: SharedString,
    pub adapter: SharedString,
    pub config: serde_json::Value,
    pub tcp_connection: Option<TcpArgumentsTemplate>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DebugAdapterBinary {
    pub command: Option<String>,
    pub arguments: Vec<String>,
    pub envs: HashMap<String, String>,
    pub cwd: Option<PathBuf>,
    pub connection: Option<TcpArguments>,
    pub request_args: StartDebuggingRequestArguments,
}
