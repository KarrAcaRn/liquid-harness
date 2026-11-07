use lh_core::{
    Config, Engine, Registry,
    agent::{self, Approval, Control},
};
use lh_protocol::json::{Value, json};
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};
use tokio::sync::mpsc;
struct Fixture {
    dir: PathBuf,
    engine: Engine,
}
impl Fixture {
    fn new(script: Value) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "lh-agent-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(dir.join("skills/test-skill")).unwrap();
        std::fs::write(
            dir.join("skills/test-skill/SKILL.md"),
            "---\nname: test-skill\ndescription: Test skill\n---\nRead before editing.\n",
        )
        .unwrap();
        let script_path = dir.join("tool.py");
        std::fs::write(&script_path,r#"import sys,json,pathlib,time
for line in sys.stdin:
 r=json.loads(line)
 if 'id' not in r: continue
 method=r['method']
 if method=='initialize': out={'protocolVersion':'2025-03-26','capabilities':{'tools':{}},'serverInfo':{'name':'fixture','version':'1'}}
 elif method=='tools/list': out={'tools':[{'name':'write_file','description':'test write','inputSchema':{'type':'object'},'annotations':{'destructiveHint':True}}]}
 elif method=='tools/call':
  args=r['params']['arguments']
  if args.get('sleep'): time.sleep(5)
  pathlib.Path(sys.argv[1]).write_text('executed')
  out={'content':[{'type':'text','text':'x'*args.get('size',4)}],'isError':False}
 else: out={}
 print(json.dumps({'jsonrpc':'2.0','id':r['id'],'result':out}),flush=True)
"#).unwrap();
        std::fs::write(dir.join("plugin.toml"),format!("[plugin]\nid='tools-fs'\nversion='1'\nprovides=['tool']\n[run]\ncommand='python3'\nargs=[{:?},{:?}]\n",script_path.to_str().unwrap(),dir.join("effect").to_str().unwrap())).unwrap();
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let mut registry = Registry::from_paths(&[
            root.join("plugins/mock-backend"),
            root.join("tests/fixtures/mock-pack"),
            dir.clone(),
        ])
        .unwrap();
        registry
            .plugins
            .get_mut("mock")
            .unwrap()
            .manifest
            .run
            .as_mut()
            .unwrap()
            .command = env!("CARGO_BIN_EXE_lh-mock-backend").into();
        registry
            .plugins
            .get_mut("mock")
            .unwrap()
            .manifest
            .config
            .insert("scripted_chat".into(), script);
        let specialist = registry.specialists.get_mut("mock-text").unwrap();
        specialist.agent = true;
        specialist.tools = vec!["tools-fs".into()];
        let mut config = Config {
            directory: dir.clone(),
            ..Default::default()
        };
        config.agent.approval_timeout_secs = 1;
        Self {
            dir,
            engine: Engine::new(registry, config),
        }
    }
    async fn run(&mut self, answer: Option<bool>) -> (lh_core::Result<Value>, Vec<Value>) {
        let route = self.engine.prepare("mock-text").await.unwrap();
        let control = Arc::new(Control::default());
        let (tx, mut rx) = mpsc::unbounded_channel();
        let job = agent::run(
            &mut self.engine,
            &route,
            vec![json!({"role":"user","content":"test"})],
            json!({}),
            tx,
            control.clone(),
        );
        tokio::pin!(job);
        let mut events = vec![];
        let result = loop {
            tokio::select! {biased;
                Some(event)=rx.recv()=>{
                    if event["event"]=="approval_required" && let Some(allow)=answer {assert!(control.approve(event["data"]["call_id"].as_str().unwrap(),allow,false).await);}
                    events.push(event);
                },
                result=&mut job=>break result,
            }
        };
        while let Ok(event) = rx.try_recv() {
            events.push(event);
        }
        (result, events)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}
fn call(id: &str, name: &str, args: Value) -> Value {
    json!({"role":"assistant","content":null,"tool_calls":[{"id":id,"type":"function","function":{"name":name,"arguments":args.to_string()}}]})
}
#[tokio::test]
async fn full_loop_skills_and_approval_allow_deny_timeout() {
    for answer in [Some(true), Some(false), None] {
        let mut f = Fixture::new(
            json!([call("s","skill_view",json!({"name":"test-skill"})),call("w","write_file",json!({})),{"role":"assistant","content":"done"}]),
        );
        let (result, events) = f.run(answer).await;
        let response = result.unwrap();
        assert_eq!(response["steps"], 3);
        assert_eq!(response["message"]["content"], "done");
        assert_eq!(f.dir.join("effect").exists(), answer == Some(true));
        assert!(events.iter().any(|e| {
            e["event"] == "tool_result"
                && e["data"]["result"]
                    .as_str()
                    .unwrap()
                    .contains("Read before editing")
        }));
        assert!(events.iter().any(|e| e["event"] == "approval_required"));
        f.engine.shutdown().await;
    }
}
#[tokio::test]
async fn auto_edit_budget_and_step_limit() {
    let mut f = Fixture::new(json!([
        call("w", "write_file", json!({"size":1000})),
        call("w2", "write_file", json!({}))
    ]));
    f.engine.config.agent.approval = Approval::AutoEdit;
    f.engine.config.agent.output_budget_bytes = 128;
    let (result, events) = f.run(None).await;
    assert!(result.unwrap_err().message.contains("budget"));
    assert!(!events.iter().any(|e| e["event"] == "approval_required"));
    assert_eq!(
        events
            .iter()
            .filter(|e| e["event"] == "tool_result")
            .count(),
        1
    );
    f.engine.shutdown().await;
    let mut f = Fixture::new(json!([call("w", "skills_list", json!({}))]));
    f.engine.config.agent.max_steps = 1;
    assert!(
        f.run(None)
            .await
            .0
            .unwrap_err()
            .message
            .contains("max steps")
    );
    f.engine.shutdown().await;
}
#[tokio::test]
async fn malformed_batch_never_executes_and_timeout_resets_process() {
    let mut batch = call("w", "write_file", json!({}));
    batch["tool_calls"].as_array_mut().unwrap().push(
        json!({"id":"bad","type":"function","function":{"name":"not_allowed","arguments":"{}"}}),
    );
    let mut f = Fixture::new(json!([batch]));
    f.engine.config.agent.approval = Approval::Yolo;
    assert!(f.run(None).await.0.is_err());
    assert!(!f.dir.join("effect").exists());
    f.engine.shutdown().await;
    let mut f = Fixture::new(
        json!([call("w","write_file",json!({"sleep":true})),{"role":"assistant","content":"done"}]),
    );
    f.engine.config.agent.approval = Approval::Yolo;
    f.engine.config.agent.tool_timeout_secs = 1;
    let (result, events) = f.run(None).await;
    assert!(result.is_ok());
    assert!(!f.dir.join("effect").exists());
    assert!(events.iter().any(|e| e["event"] == "tool_result"
        && e["data"]["result"].as_str().unwrap().contains("timed out")));
    assert_ne!(
        f.engine.process_status().await["tools-fs"]["state"],
        "running"
    );
    f.engine.shutdown().await;
}
