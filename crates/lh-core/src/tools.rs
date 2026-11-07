//! MCP tool discovery and dispatch. Approval policy belongs to the caller.
use crate::{Engine, Error, Result};
use lh_protocol::json::{Value, json};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Serialize, Clone, Debug)]
pub struct Tool {
    pub name: String,
    pub plugin: String,
    pub original_name: String,
    pub description: String,
    pub input_schema: Value,
    pub destructive: bool,
}
impl Tool {
    pub fn schema(&self) -> Value {
        json!({"type":"function","function":{"name":self.name,
            "description":self.description,"parameters":self.input_schema}})
    }
}

/// Discover enabled executable MCP plugins. Selectors match plugin IDs, original
/// tool names, or collision-qualified names. An empty selection includes all.
/// Listing all enabled plugins keeps exposed names stable across selections.
pub async fn discover(engine: &mut Engine, selected: &[String]) -> Result<Vec<Tool>> {
    let plugins: Vec<String> = engine
        .registry
        .plugins
        .iter()
        .filter(|(id, entry)| {
            engine.enabled(id)
                && entry.manifest.run.is_some()
                && entry.manifest.plugin.provides.iter().any(|p| p == "tool")
        })
        .map(|(id, _)| id.clone())
        .collect();
    let mut tools = Vec::new();
    for plugin in plugins {
        let host = engine.host(&plugin)?;
        let mut cursor = None;
        let mut cursors = BTreeSet::new();
        let mut names = BTreeSet::new();
        loop {
            let params = match &cursor {
                Some(cursor) => json!({"cursor":cursor}),
                None => json!({}),
            };
            let page = host.call("tools/list", params).await?;
            let entries = page["tools"].as_array().ok_or_else(|| {
                Error::new(
                    -32603,
                    format!("tool plugin '{plugin}' returned invalid tools/list"),
                )
            })?;
            for entry in entries {
                let name = entry["name"]
                    .as_str()
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| Error::new(-32603, "MCP tool has no name"))?;
                if !names.insert(name.to_owned()) || !entry["inputSchema"].is_object() {
                    return Err(Error::new(
                        -32603,
                        format!("invalid or duplicate MCP tool '{plugin}/{name}'"),
                    ));
                }
                tools.push(Tool {
                    name: name.into(),
                    plugin: plugin.clone(),
                    original_name: name.into(),
                    description: entry["description"].as_str().unwrap_or_default().into(),
                    input_schema: entry["inputSchema"].clone(),
                    // MCP defaults to destructive unless explicitly read-only.
                    destructive: entry["annotations"]["readOnlyHint"] != true
                        && entry["annotations"]["destructiveHint"]
                            .as_bool()
                            .unwrap_or(true),
                });
            }
            match page.get("nextCursor") {
                None | Some(Value::Null) => break,
                Some(Value::String(next)) if !next.is_empty() && cursors.insert(next.clone()) => {
                    cursor = Some(next.clone());
                }
                _ => {
                    return Err(Error::new(
                        -32603,
                        "invalid or repeated MCP tools/list cursor",
                    ));
                }
            }
        }
    }
    qualify(&mut tools)?;
    for selector in selected {
        if !tools.iter().any(|t| matches_selector(t, selector))
            && !engine.registry.plugins.get(selector).is_some_and(|e| {
                engine.enabled(selector)
                    && e.manifest.run.is_some()
                    && e.manifest.plugin.provides.iter().any(|p| p == "tool")
            })
        {
            return Err(Error::new(
                -32602,
                format!("unknown or disabled tool selector '{selector}'"),
            ));
        }
    }
    tools.retain(|t| selected.is_empty() || selected.iter().any(|s| matches_selector(t, s)));
    Ok(tools)
}
fn matches_selector(tool: &Tool, selector: &str) -> bool {
    tool.plugin == selector || tool.original_name == selector || tool.name == selector
}
fn qualify(tools: &mut [Tool]) -> Result<()> {
    let mut counts = BTreeMap::new();
    for tool in tools.iter() {
        *counts.entry(tool.original_name.clone()).or_insert(0) += 1;
    }
    for tool in tools.iter_mut() {
        if counts[&tool.original_name] > 1
            || matches!(tool.original_name.as_str(), "skills_list" | "skill_view")
        {
            tool.name = format!("{}__{}", tool.plugin, tool.original_name);
        }
    }
    // A raw name can itself match a generated qualified name. Qualify that raw
    // name too, rather than silently overriding a tool in the model's namespace.
    loop {
        let mut counts = BTreeMap::new();
        for tool in tools.iter() {
            *counts.entry(tool.name.clone()).or_insert(0) += 1;
        }
        if counts.values().all(|n| *n == 1) {
            return Ok(());
        }
        let mut changed = false;
        for tool in tools.iter_mut() {
            if counts[&tool.name] > 1 && tool.name == tool.original_name {
                tool.name = format!("{}__{}", tool.plugin, tool.original_name);
                changed = true;
            }
        }
        if !changed {
            return Err(Error::new(-32602, "ambiguous qualified MCP tool names"));
        }
    }
}

/// Return the MCP result unchanged, including content and isError. This function
/// does not grant approval: callers must enforce their execution policy first.
pub async fn call(engine: &mut Engine, tool: &Tool, args: Value) -> Result<Value> {
    if !args.is_object() {
        return Err(Error::new(-32602, "tool arguments must be an object"));
    }
    if !engine
        .registry
        .plugins
        .get(&tool.plugin)
        .is_some_and(|entry| {
            entry.manifest.run.is_some()
                && entry.manifest.plugin.provides.iter().any(|p| p == "tool")
        })
    {
        return Err(Error::new(-32602, "tool plugin is not executable"));
    }
    engine
        .host(&tool.plugin)?
        .call(
            "tools/call",
            json!({"name":tool.original_name,"arguments":args}),
        )
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn discovery_pagination_selection_and_original_name_dispatch() {
        let script = r#"
import json, sys
ready = False
for line in sys.stdin:
    r = json.loads(line)
    method = r['method']
    if method == 'initialize':
        result = {'protocolVersion':'2025-03-26','serverInfo':{'name':'test','version':'1'},'capabilities':{'tools':{}}}
    elif method == 'notifications/initialized':
        ready = True
        continue
    else:
        assert ready
        if method == 'tools/list':
            page = r['params'].get('cursor')
            result = {'tools':[{'name':'skill_view' if page else 'echo','inputSchema':{'type':'object'},'annotations':{'destructiveHint':False}}]}
            if not page:
                result['nextCursor'] = 'second'
        else:
            assert method == 'tools/call'
            result = {'content':[{'type':'text','text':r['params']['name']}],'isError':False,'arguments':r['params']['arguments']}
    print(json.dumps({'jsonrpc':'2.0','id':r['id'],'result':result}), flush=True)
"#;
        let mut registry = crate::Registry::default();
        for id in ["one", "two"] {
            // Both tags allow this test to run before Engine's generalized host
            // lookup lands; Host deliberately gives MCP precedence.
            let mut manifest: lh_protocol::Manifest = lh_protocol::toml::from_str(&format!(
                r#"
[plugin]
id = "{id}"
version = "1"
protocol = 1
provides = ["tool"]
[run]
command = "python3"
"#
            ))
            .unwrap();
            manifest.run.as_mut().unwrap().args = vec!["-u".into(), "-c".into(), script.into()];
            registry.plugins.insert(
                id.into(),
                crate::registry::Entry {
                    path: std::env::temp_dir().join("nonexistent-plugin/plugin.toml"),
                    manifest,
                },
            );
        }
        let mut engine = Engine::new(registry, crate::Config::default());
        let tools = discover(&mut engine, &[]).await.unwrap();
        assert_eq!(tools.len(), 4);
        assert_eq!(tools[0].name, "one__echo");
        assert_eq!(tools[1].name, "one__skill_view");
        assert!(!tools[0].destructive);
        let result = call(&mut engine, &tools[0], json!({"hello":true}))
            .await
            .unwrap();
        assert_eq!(result["content"][0]["text"], "echo");
        assert_eq!(result["arguments"], json!({"hello":true}));
        assert_eq!(
            discover(&mut engine, &["one".into()]).await.unwrap().len(),
            2
        );
        assert_eq!(
            discover(&mut engine, &["echo".into()]).await.unwrap().len(),
            2
        );
        assert_eq!(
            discover(&mut engine, &["two__echo".into()])
                .await
                .unwrap()
                .len(),
            1
        );
        assert!(discover(&mut engine, &["missing".into()]).await.is_err());
        assert!(call(&mut engine, &tools[0], Value::Null).await.is_err());
        engine
            .registry
            .plugins
            .get_mut("two")
            .unwrap()
            .manifest
            .plugin
            .enabled = false;
        assert_eq!(discover(&mut engine, &[]).await.unwrap().len(), 2);
        assert!(call(&mut engine, &tools[2], json!({})).await.is_err());
        engine.shutdown().await;
    }

    fn tool(plugin: &str, name: &str) -> Tool {
        Tool {
            name: name.into(),
            plugin: plugin.into(),
            original_name: name.into(),
            description: "test".into(),
            input_schema: json!({"type":"object"}),
            destructive: false,
        }
    }
    #[test]
    fn collisions_reserved_names_and_schemas() {
        let mut tools = vec![
            tool("a", "read"),
            tool("b", "read"),
            tool("a", "skills_list"),
            tool("a", "skill_view"),
            tool("c", "a__read"),
        ];
        qualify(&mut tools).unwrap();
        assert_eq!(
            tools.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(),
            vec![
                "a__read",
                "b__read",
                "a__skills_list",
                "a__skill_view",
                "c__a__read"
            ]
        );
        assert_eq!(tools[0].original_name, "read");
        assert_eq!(
            tools[0].schema(),
            json!({"type":"function","function":{
            "name":"a__read","description":"test","parameters":{"type":"object"}}})
        );
        assert!(matches_selector(&tools[0], "a"));
        assert!(matches_selector(&tools[0], "read"));
        assert!(matches_selector(&tools[0], "a__read"));
    }
}
