use super::*;
use windows_tools_core::{Execution, Lease, RpcResult, Session, Target};

const TOKEN: &str = "a-token-of-at-least-thirty-two-characters";

struct Native;
impl Execution for Native {
    fn run(&self, _: serde_json::Value, _: &dyn Fn() -> bool) -> RpcResult {
        Ok(serde_json::json!({}))
    }
}
impl Session for Native {
    fn describe(&self, target: &Target, _: bool) -> serde_json::Value {
        serde_json::json!({"target": target})
    }
}

#[test]
fn only_the_token_is_let_in() {
    let base = Target {
        host_instance: "instance".into(),
        control_epoch: 0,
    };
    let lease = Lease::new(std::time::Duration::from_secs(5));
    let tools = Arc::new(Toolset::new(
        base,
        lease,
        Box::new(Native),
        Box::new(Native),
    ));
    let a = Adapter::new(tools, "https://127.0.0.1:8767".into(), None, TOKEN);
    assert!(a.authenticated(TOKEN));
    assert!(!a.authenticated("another-token-of-at-least-thirty-two-chars"));
    assert!(!a.authenticated(""));
}
#[test]
fn the_tool_list_is_the_two_model_facing_tools() {
    let names: Vec<_> = catalog::TOOLS
        .iter()
        .map(|tool| tool.name.to_string())
        .collect();
    assert_eq!(names, ["windows_describe", "windows_exec_run"]);
    let schema = serde_json::to_value(&catalog::TOOLS[1].input_schema).unwrap();
    assert_eq!(schema["required"], serde_json::json!(["command"]));
}
