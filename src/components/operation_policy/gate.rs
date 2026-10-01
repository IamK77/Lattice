use std::collections::{HashMap, HashSet};

use serde_json::{json, Value};

use crate::components::{browser_tools, expert_definitions, interface_permissions, trust_policy};
use crate::contracts::event::EventTypeDecl;
use crate::{
    core_events as ce, Component, ComponentManifest, Ctx, EventDraft, EventEnvelope, PortDecl,
};

use super::{
    read_grants, AuthorizationSources, CommandRule, Decision, FlowGrant, GrantMatcher, GrantState,
    Invocation, AUTH_REQUESTED, CHANNEL, DECISION, NAME, STATE,
};

pub fn manifest() -> ComponentManifest {
    let mut manifest = trust_policy::manifest();
    manifest.name = NAME.into();
    manifest.entry = format!("builtin:{NAME}");
    manifest.inputs = vec![
        PortDecl::new("review", &[ce::TOOL_EXEC_STARTED]),
        PortDecl::new("answer", &[ce::EXTERNAL_INPUT]),
        PortDecl::new("control", &[ce::INTERRUPTED]),
    ];
    manifest.outputs = vec![
        PortDecl::new("forward", &[ce::TOOL_EXEC_STARTED]),
        PortDecl::new("verdict", &[ce::TOOL_EXEC_COMPLETED]),
        PortDecl::new("request", &[AUTH_REQUESTED]),
        PortDecl::new("decision", &[DECISION]),
        PortDecl::new("state", &[STATE]),
        PortDecl::new("answered", &[ce::EXTERNAL_INPUT]),
        PortDecl::new("interrupted", &[ce::INTERRUPTED]),
    ];
    manifest.events = vec![
        EventTypeDecl::new(AUTH_REQUESTED, "An operation needs human authorization")
            .with_schema(json!({"type":"object", "required":["request","held","tool","summary","grants"],
                "properties":{"request":{"type":"string"},"held":{"type":"string"},"tool":{"type":"string"},
                    "summary":{"type":"string"},"grants":{"type":"array"}}})),
        EventTypeDecl::decision(DECISION, "An operation authorization decision")
            .with_schema(json!({"type":"object", "required":["verdict"],
                "properties":{"verdict":{"enum":["granted","denied","cancelled"]}}})),
        EventTypeDecl::decision(STATE, "Flow-scoped operation grants changed")
            .with_schema(json!({"type":"object", "required":["grants"], "properties":{"grants":{"type":"object"}}})),
    ];
    manifest
}

struct Pending {
    request: EventEnvelope,
    proposals: Vec<GrantMatcher>,
}

pub struct OperationPolicy {
    ask: bool,
    shell_tools: Vec<String>,
    rules: Vec<CommandRule>,
    sources: AuthorizationSources,
    controllers: Vec<String>,
    grants: GrantState,
    pending: HashMap<String, Pending>,
    answered_questions: HashSet<String>,
    restored_through: u64,
    settled: HashSet<String>,
    invalid: Option<String>,
}

impl OperationPolicy {
    pub fn from_config(config: Option<&Value>) -> Self {
        let rules = config
            .and_then(|c| c.get("rules"))
            .cloned()
            .unwrap_or_else(|| json!([]));
        let rules = serde_json::from_value::<Vec<CommandRule>>(rules);
        let invalid = rules.as_ref().err().map(ToString::to_string).or_else(|| {
            rules
                .as_ref()
                .ok()
                .filter(|rules| rules.iter().any(|rule| rule.pattern.is_empty()))
                .map(|_| "command rule patterns cannot be empty".into())
        });
        let strings = |key: &str, default: &[&str]| -> Result<Vec<String>, String> {
            let value = config
                .and_then(|c| c.get(key))
                .cloned()
                .unwrap_or_else(|| json!(default));
            let names: Vec<String> =
                serde_json::from_value(value).map_err(|e| format!("{key}: {e}"))?;
            if names.iter().any(String::is_empty) {
                return Err(format!("{key} cannot contain empty names"));
            }
            Ok(names)
        };
        let shell_tools = strings("shellTools", &["Run"]);
        let controllers = strings("controllers", &["ui"]);
        let invalid = invalid
            .or_else(|| shell_tools.as_ref().err().cloned())
            .or_else(|| controllers.as_ref().err().cloned());
        Self {
            ask: config.is_some_and(|c| c["stance"] == "ask"),
            shell_tools: shell_tools.unwrap_or_default(),
            rules: rules.unwrap_or_default(),
            sources: AuthorizationSources::from_config(config),
            controllers: controllers.unwrap_or_default(),
            grants: GrantState::default(),
            pending: HashMap::new(),
            answered_questions: HashSet::new(),
            restored_through: 0,
            settled: HashSet::new(),
            invalid,
        }
    }

    fn publish(&self, cause: Option<&EventEnvelope>, reason: &str, ctx: &mut Ctx) {
        let causes: Vec<_> = cause.into_iter().map(|e| e.id.as_str()).collect();
        ctx.emit(
            "state",
            EventDraft::new(STATE, &causes, json!(self.grants)).with_reason(reason),
        );
    }

    fn forward(
        &mut self,
        request: &EventEnvelope,
        answer: Option<&EventEnvelope>,
        reason: &str,
        evidence: Value,
        ctx: &mut Ctx,
    ) {
        let mut causes = vec![request.id.as_str()];
        if let Some(answer) = answer {
            causes.push(&answer.id);
        }
        ctx.emit(
            "decision",
            EventDraft::new(
                DECISION,
                &causes,
                json!({"held":request.id,"verdict":"granted","authorization":evidence}),
            )
            .with_reason(reason),
        );
        ctx.emit(
            "forward",
            EventDraft::new(ce::TOOL_EXEC_STARTED, &causes, request.payload.clone()),
        );
        self.settled.insert(request.id.clone());
    }

    fn deny(
        &mut self,
        request: &EventEnvelope,
        answer: Option<&EventEnvelope>,
        message: &str,
        ctx: &mut Ctx,
    ) {
        let mut causes = vec![request.id.as_str()];
        if let Some(answer) = answer {
            causes.push(&answer.id);
        }
        ctx.emit(
            "decision",
            EventDraft::new(
                DECISION,
                &causes,
                json!({"held":request.id,"verdict":"denied"}),
            )
            .with_reason(message),
        );
        ctx.emit("verdict", EventDraft::new(ce::TOOL_EXEC_COMPLETED, &causes, json!({
            "call":request.payload["call"],"status":"error",
            "error":{"code":"operation.denied","message":message,"blame":"request","retryable":false,"transient":false}
        })));
        self.settled.insert(request.id.clone());
    }

    fn review(&mut self, request: &EventEnvelope, ctx: &mut Ctx) -> Result<(), String> {
        if self.settled.contains(&request.id)
            || self.pending.contains_key(&request.id)
            || ctx
                .log()
                .has_outcome(&request.id)
                .map_err(|e| e.to_string())?
        {
            return Ok(());
        }
        if self.stopped(request, ctx)? {
            self.interrupt(request, ctx)?;
            return Ok(());
        }
        if let Some(problem) = self.invalid.clone() {
            self.deny(
                request,
                None,
                &format!("Operation policy is unavailable: {problem}"),
                ctx,
            );
            return Ok(());
        }
        let tool = request.payload["tool"].as_str().unwrap_or_default();
        if !self.shell_tools.iter().any(|name| name == tool) {
            // Existing providers keep their own confirmation and validation.
            ctx.emit(
                "forward",
                EventDraft::new(
                    ce::TOOL_EXEC_STARTED,
                    &[&request.id],
                    request.payload.clone(),
                ),
            );
            self.settled.insert(request.id.clone());
            return Ok(());
        }
        let arguments = &request.payload["arguments"];
        let Some(script) = arguments["command"].as_str() else {
            self.deny(
                request,
                None,
                "A shell operation requires a command string",
                ctx,
            );
            return Ok(());
        };
        let invocation = Invocation::parse(script);
        let decision = invocation.decision(&self.rules, &self.grants.matchers(), tool, arguments);
        if decision == Decision::Forbidden {
            self.deny(
                request,
                None,
                "A configured command rule forbids this operation",
                ctx,
            );
            return Ok(());
        }
        if decision == Decision::Allow {
            self.forward(
                request,
                None,
                "All commands are covered by operation rules",
                json!({"scope":"flow"}),
                ctx,
            );
            return Ok(());
        }
        if let Some(evidence) =
            interface_permissions::allowance(ctx.log(), request, &self.sources.interfaces)
                .map_err(|e| e.to_string())?
        {
            self.forward(
                request,
                None,
                "A contributing source interface has live permission",
                json!({"scope":"interface","evidence":evidence}),
                ctx,
            );
            return Ok(());
        }
        if !self.ask {
            self.deny(
                request,
                None,
                "This operation is not granted and the unattended policy does not ask",
                ctx,
            );
            return Ok(());
        }
        // An explicit prompt rule outranks a stored allow rule. Do not offer a
        // persistent grant which could never satisfy that configured rule.
        let forced_prompt = invocation.commands.iter().any(|argv| {
            self.rules
                .iter()
                .any(|rule| rule.decision == Decision::Prompt && rule.matches(argv))
        });
        let proposals = if forced_prompt {
            vec![]
        } else {
            invocation.proposals(tool, arguments)
        };
        self.pending.insert(
            request.id.clone(),
            Pending {
                request: request.clone(),
                proposals: proposals.clone(),
            },
        );
        ctx.emit("request", EventDraft::new(AUTH_REQUESTED, &[&request.id], json!({
            "request":request.id,"held":request.id,"key":request.id,"tool":tool,
            "summary":format!("Execute this shell operation: {script}"),
            "commands":invocation.commands,"decomposed":invocation.decomposed,"grants":proposals,
            "grantScope":"This flow, including after reopening. Prefix grants do not restrict trailing arguments, working directory, or executable contents."
        })));
        Ok(())
    }

    fn stopped(&self, request: &EventEnvelope, ctx: &Ctx) -> Result<bool, String> {
        if ctx
            .log()
            .has_outcome(&request.id)
            .map_err(|e| e.to_string())?
        {
            return Ok(true);
        }
        // Gates can forward an old request after the stop has been delivered.
        // Follow copies of this call only, not unrelated historical work.
        let mut since = request.seq;
        let mut pending = request.causes.clone();
        let mut seen = HashSet::new();
        while let Some(id) = pending.pop() {
            if !seen.insert(id.clone()) {
                continue;
            }
            let parent = ctx
                .log()
                .get(&id)
                .map_err(|e| e.to_string())?
                .ok_or_else(|| format!("Missing operation ancestor: {id}"))?;
            if parent.event_type == ce::TOOL_EXEC_STARTED
                && parent.payload["call"] == request.payload["call"]
                && parent.payload["tool"] == request.payload["tool"]
            {
                since = since.min(parent.seq);
                pending.extend(parent.causes);
            }
        }
        ctx.log()
            .scan_back_types(&[ce::INTERRUPTED], |event, _| {
                // A call-specific outcome is not a new global stop.
                Ok((event.seq > since
                    && event.payload["by"] == "user"
                    && event.payload.get("call").is_none())
                .then_some(()))
            })
            .map(|e| e.is_some())
            .map_err(|e| e.to_string())
    }

    fn interrupt(&mut self, request: &EventEnvelope, ctx: &mut Ctx) -> Result<(), String> {
        if !ctx
            .log()
            .has_outcome(&request.id)
            .map_err(|e| e.to_string())?
        {
            ctx.emit(
                "interrupted",
                EventDraft::new(
                    ce::INTERRUPTED,
                    &[&request.id],
                    json!({"by":"user","call":request.payload["call"]}),
                )
                .with_reason("The user stopped this operation before authorization completed"),
            );
        }
        self.settled.insert(request.id.clone());
        Ok(())
    }

    fn answer(&mut self, answer: &EventEnvelope, ctx: &mut Ctx) -> Result<(), String> {
        if !self.controllers.contains(&answer.source) {
            return Ok(());
        }
        if let Some(problem) = &self.invalid {
            return Err(format!("Operation policy is unavailable: {problem}"));
        }
        if answer.payload["channel"] == CHANNEL {
            if answer.payload["action"] == "revoke" {
                if let Some(id) = answer.payload["grant"].as_str() {
                    if self.grants.grants.remove(id).is_some() {
                        self.publish(
                            Some(answer),
                            "The user revoked a flow-scoped operation grant",
                            ctx,
                        );
                    }
                }
            }
            return Ok(());
        }
        let scoped = answer.payload["channel"] == super::ANSWER_CHANNEL;
        if !scoped && answer.payload["channel"] != trust_policy::AUTH_CHANNEL {
            return Ok(());
        }
        if let Some(scope) = answer
            .payload
            .get("scope")
            .or_else(|| scoped.then_some(&Value::Null))
        {
            if !matches!(scope.as_str(), Some("once" | "flow")) {
                ctx.emit(
                    "decision",
                    EventDraft::new(
                        DECISION,
                        &[&answer.id],
                        json!({
                            "verdict":"denied", "answer":answer.id,
                            "error":"Authorization scope must be once or flow when supplied"
                        }),
                    )
                    .with_reason(
                        "Rejected an invalid answer without consuming the pending authorization",
                    ),
                );
                return Ok(());
            }
        }
        let Some(id) = answer.payload["request"].as_str() else {
            return Ok(());
        };
        let Some(question) = ctx.log().get(id).map_err(|e| e.to_string())? else {
            return Ok(());
        };
        if question.event_type == AUTH_REQUESTED {
            let Some(request_id) = question.payload["request"].as_str() else {
                return Ok(());
            };
            let Some(pending) = self.pending.remove(request_id) else {
                return Ok(());
            };
            if self.stopped(&pending.request, ctx)? {
                self.interrupt(&pending.request, ctx)?;
                return Ok(());
            }
            if answer.payload["approve"] != true {
                self.deny(
                    &pending.request,
                    Some(answer),
                    "The user refused this operation. Do not retry it through another tool.",
                    ctx,
                );
            } else if answer.payload["scope"] == "flow" && pending.proposals.is_empty() {
                self.deny(
                    &pending.request,
                    Some(answer),
                    "This configured prompt cannot be replaced with a persistent allow rule",
                    ctx,
                );
            } else {
                if answer.payload["scope"] == "flow" {
                    self.save_grant(answer, &question, pending.proposals, ctx);
                }
                self.forward(
                    &pending.request,
                    Some(answer),
                    "The user approved this operation",
                    json!({"scope":answer.payload["scope"].as_str().unwrap_or("once")}),
                    ctx,
                );
            }
        } else if matches!(
            question.event_type.as_str(),
            trust_policy::AUTH_REQUESTED
                | browser_tools::AUTH_REQUESTED
                | expert_definitions::AUTH_REQUESTED
        ) && question.seq > self.restored_through
            && self.answered_questions.insert(question.id.clone())
        {
            let Some(request_id) = question.payload["request"].as_str() else {
                return Ok(());
            };
            let Some(request) = ctx.log().get(request_id).map_err(|e| e.to_string())? else {
                return Ok(());
            };
            if self.stopped(&request, ctx)? {
                return Ok(());
            }
            let mut payload = answer.payload.clone();
            payload["channel"] = json!(trust_policy::AUTH_CHANNEL);
            if payload["approve"] == true && payload["scope"] == "flow" {
                let tool = request.payload["tool"]
                    .as_str()
                    .ok_or("authorization request has no tool")?;
                self.save_grant(
                    answer,
                    &question,
                    vec![GrantMatcher::ExactArguments {
                        tool: tool.into(),
                        arguments: request.payload["arguments"].clone(),
                        effects: question.payload.get("effects").cloned(),
                    }],
                    ctx,
                );
            }
            // Explicit once/flow approvals must not become permanent trust.
            // An older answer with no scope keeps the existing trust semantics.
            if matches!(payload["scope"].as_str(), Some("once" | "flow")) {
                payload["persistTrust"] = json!(false);
            }
            ctx.emit(
                "answered",
                EventDraft::new(ce::EXTERNAL_INPUT, &[&answer.id], payload),
            );
        }
        Ok(())
    }

    fn save_grant(
        &mut self,
        answer: &EventEnvelope,
        question: &EventEnvelope,
        matchers: Vec<GrantMatcher>,
        ctx: &mut Ctx,
    ) {
        self.grants.grants.insert(
            answer.id.clone(),
            FlowGrant {
                matchers,
                question: question.id.clone(),
                interface: answer.payload["interface"].as_str().map(str::to_owned),
            },
        );
        self.publish(
            Some(answer),
            "The user saved an operation grant in this flow",
            ctx,
        );
    }
}

impl Component for OperationPolicy {
    fn restore(&mut self, ctx: &mut Ctx) {
        self.restored_through = ctx.log().snapshot_end();
        match read_grants(ctx.log(), &self.sources.operations) {
            Ok(grants) => {
                self.grants = grants;
                self.publish(
                    None,
                    "Rebuilt flow grants without restoring pending calls or interface permission",
                    ctx,
                );
            }
            Err(error) => {
                self.invalid = Some(error.to_string());
                ctx.fail("restore flow grants", error.to_string(), &[]);
            }
        }
    }

    fn handle(&mut self, port: &str, event: &EventEnvelope, ctx: &mut Ctx) {
        let result = match port {
            "review" => self.review(event, ctx),
            "answer" => self.answer(event, ctx),
            "control" => {
                let pending = std::mem::take(&mut self.pending);
                for (id, pending) in pending {
                    match self.stopped(&pending.request, ctx) {
                        Ok(true) => {
                            if let Err(error) = self.interrupt(&pending.request, ctx) {
                                ctx.fail(
                                    "settle stopped operation",
                                    error,
                                    std::slice::from_ref(&id),
                                );
                                return;
                            }
                        }
                        Ok(false) => {
                            self.pending.insert(id, pending);
                        }
                        Err(error) => {
                            ctx.fail("read operation outcome", error, std::slice::from_ref(&id));
                            return;
                        }
                    }
                }
                Ok(())
            }
            _ => Ok(()),
        };
        if let Err(error) = result {
            ctx.fail(
                "operation authorization",
                error,
                std::slice::from_ref(&event.id),
            );
        }
    }
}
