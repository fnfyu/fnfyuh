//! Deterministic projection-to-model context compiler.
//!
//! It accepts all dynamic inputs explicitly and never reads a clock, environment,
//! filesystem, or random source. Provider cache is an optimization; `digest` is the
//! correctness/audit identity of the compiled context.

use harness_protocol::{canonical_json, EventEnvelope, EventPayload, ToolIntent};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const COMPILER_VERSION: &str = "prompt-compiler.v1";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PromptLayers {
    pub system_rules: String,
    pub tool_schema: String,
    pub project_rules: String,
    pub frozen_summaries: Vec<String>,
}

impl Default for PromptLayers {
    fn default() -> Self {
        Self {
            system_rules: String::new(),
            tool_schema: String::new(),
            project_rules: String::new(),
            frozen_summaries: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelMessage {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CompiledPrompt {
    pub compiler_version: String,
    pub messages: Vec<ModelMessage>,
    pub stable_prefix: String,
    pub dynamic_tail: Vec<ModelMessage>,
    pub digest: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub input_schema: String,
}

pub fn compile(
    events: &[EventEnvelope],
    layers: &PromptLayers,
    tools: &[ToolDefinition],
) -> CompiledPrompt {
    let stable_prefix = stable_prefix(layers, tools);
    let mut messages = Vec::new();
    let mut dynamic_tail = Vec::new();

    if !stable_prefix.is_empty() {
        messages.push(ModelMessage {
            role: "system".to_string(),
            content: stable_prefix.clone(),
        });
    }

    for event in events {
        match &event.payload {
            EventPayload::UserMessage { content, .. } => push_message(
                &mut messages,
                &mut dynamic_tail,
                "user",
                content.clone(),
            ),
            EventPayload::ModelResponded { content, .. } => push_message(
                &mut messages,
                &mut dynamic_tail,
                "assistant",
                content.clone(),
            ),
            EventPayload::ToolProposed {
                tool_name, intent, ..
            } => push_message(
                &mut messages,
                &mut dynamic_tail,
                "assistant",
                format!("[tool proposed: {tool_name}] {}", intent_summary(intent)),
            ),
            EventPayload::ToolFinished { result, .. } => push_message(
                &mut messages,
                &mut dynamic_tail,
                "tool",
                format!(
                    "[tool result: {:?}]\nstdout_digest: {} ({} bytes)\nstderr_digest: {} ({} bytes)",
                    result.status,
                    result.stdout_digest,
                    result.stdout_bytes,
                    result.stderr_digest,
                    result.stderr_bytes
                ),
            ),
            EventPayload::ToolFailed { error_code, message, .. } => push_message(
                &mut messages,
                &mut dynamic_tail,
                "tool",
                format!("[tool failure: {error_code}] {message}"),
            ),
            EventPayload::ContextCompacted { summary, .. } => {
                let compacted = ModelMessage {
                    role: "summary".to_string(),
                    content: summary.clone(),
                };
                messages.retain(|message| message.role == "system");
                dynamic_tail.clear();
                messages.push(compacted.clone());
                dynamic_tail.push(compacted);
            }
            _ => {}
        }
    }

    let digest_input = (&stable_prefix, &messages, COMPILER_VERSION);
    let digest = digest_of(&digest_input);
    CompiledPrompt {
        compiler_version: COMPILER_VERSION.to_string(),
        messages,
        stable_prefix,
        dynamic_tail,
        digest,
    }
}

fn push_message(
    messages: &mut Vec<ModelMessage>,
    dynamic_tail: &mut Vec<ModelMessage>,
    role: &str,
    content: String,
) {
    let message = ModelMessage {
        role: role.to_string(),
        content,
    };
    messages.push(message.clone());
    dynamic_tail.push(message);
}

fn stable_prefix(layers: &PromptLayers, tools: &[ToolDefinition]) -> String {
    let mut sorted_tools = tools.to_vec();
    sorted_tools.sort_by(|left, right| left.name.cmp(&right.name));
    let generated_tool_schema = sorted_tools
        .iter()
        .map(|tool| {
            format!(
                "{}\n{}\n{}",
                tool.name, tool.description, tool.input_schema
            )
        })
        .collect::<Vec<_>>()
        .join("\n---\n");
    let tool_schema = if layers.tool_schema.is_empty() {
        generated_tool_schema
    } else {
        layers.tool_schema.clone()
    };
    let frozen_summaries = layers.frozen_summaries.join("\n");
    [
        layers.system_rules.as_str(),
        tool_schema.as_str(),
        layers.project_rules.as_str(),
        frozen_summaries.as_str(),
    ]
    .into_iter()
    .filter(|part| !part.is_empty())
    .collect::<Vec<_>>()
    .join("\n\n")
}

fn intent_summary(intent: &ToolIntent) -> String {
    match intent {
        ToolIntent::ReadFile { path } => format!("read {path}"),
        ToolIntent::Search { root, query } => format!("search {root} for {query}"),
        ToolIntent::ListFiles { root, depth, .. } => format!("list {root} depth {depth}"),
        ToolIntent::ReadImage { path, .. } => format!("read image {path}"),
        ToolIntent::WriteFile { path, .. } => format!("write {path}"),
        ToolIntent::EditFile { path, .. } => format!("edit {path}"),
        ToolIntent::Exec { program, args, .. } => format!("exec {program} {}", args.join(" ")),
        ToolIntent::Git { args, .. } => format!("git {}", args.join(" ")),
        ToolIntent::Test { program, args, .. } => format!("test {program} {}", args.join(" ")),
    }
}

fn digest_of<T: Serialize>(value: &T) -> String {
    let encoded = canonical_json(value).expect("prompt values are serializable");
    let digest = Sha256::digest(encoded.as_bytes());
    format!("sha256:{digest:x}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use harness_protocol::{EventEnvelope, EventPayload};

    fn event(sequence: i64, payload: EventPayload) -> EventEnvelope {
        EventEnvelope::new("session", sequence, payload, None, None)
    }

    #[test]
    fn stable_prefix_and_digest_are_deterministic() {
        let events = vec![
            event(
                1,
                EventPayload::UserMessage {
                    turn_id: "t".into(),
                    content: "hello".into(),
                },
            ),
            event(
                2,
                EventPayload::ModelResponded {
                    turn_id: "t".into(),
                    run_id: "r".into(),
                    request_id: "q".into(),
                    content: "world".into(),
                    stop_reason: None,
                },
            ),
        ];
        let layers = PromptLayers {
            system_rules: "rules".into(),
            tool_schema: "schema".into(),
            project_rules: "project".into(),
            frozen_summaries: vec!["summary".into()],
        };
        let tools = vec![ToolDefinition {
            name: "read".into(),
            description: "read a file".into(),
            input_schema: "{}".into(),
        }];
        let first = compile(&events, &layers, &tools);
        let second = compile(&events, &layers, &tools);
        assert_eq!(first, second);
        assert_eq!(first.messages.len(), 3);
        assert!(first.digest.starts_with("sha256:"));
    }

    #[test]
    fn compaction_replaces_old_dynamic_messages() {
        let events = vec![
            event(
                1,
                EventPayload::UserMessage {
                    turn_id: "t".into(),
                    content: "old".into(),
                },
            ),
            event(
                2,
                EventPayload::ContextCompacted {
                    turn_id: "t".into(),
                    summary: "frozen".into(),
                    summarized_through_sequence: 1,
                },
            ),
        ];
        let prompt = compile(&events, &PromptLayers::default(), &[]);
        assert_eq!(prompt.messages[0].role, "summary");
        assert_eq!(prompt.messages[0].content, "frozen");
        assert_eq!(prompt.dynamic_tail.len(), 1);
    }
}
