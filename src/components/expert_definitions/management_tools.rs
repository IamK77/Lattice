use crate::experts::management::{DELETE, LIST, SAVE};
use serde_json::{json, Value};

pub(super) fn declarations() -> Vec<Value> {
    let target = json!({"type":"object","properties":{
        "scope":{"type":"string","enum":["project","personal"]},"root":{"type":"string"},"id":{"type":"string"}},
        "required":["scope","root","id"],"additionalProperties":false});
    let base = json!({"operation":{"type":"string"},"target":target,"fileVersion":{"type":["string","null"]},
        "expectedActivation":{"type":["object","null"]},"reason":{"type":"string","minLength":1}});
    let mut save = base.clone();
    save["operation"] = json!({"type":"string","enum":["put"]});
    save["definition"] = json!({"type":"object"});
    let mut delete = base;
    delete["operation"] = json!({"type":"string","enum":["delete"]});
    delete["fileVersion"] = json!({"type":"string"});
    vec![
        json!({"name":LIST,"description":"List built-in, project and personal experts, their readiness, model choices and source roots. Use InspectExpert for current content and mutation arguments.",
            "parameters":{"type":"object","properties":{},"additionalProperties":false},
            "effects":{"reads":["expert definitions, model catalog and authorization evidence"],"writes":["expert state locks and process credential references"],"network":[],"executes":false}}),
        json!({"name":SAVE,"description":"Create, update or copy a reusable expert definition after reviewing the full content. Inspect the destination first: fileVersion=null explicitly expects absence. Supply the inspected activation state unchanged. Copies carry full proposed content, not a mutable source path, and do not inherit activation. Saving withdraws old availability; call ActivateExpert with the returned arguments to enable the saved revision. Conflicts and partial outcomes require fresh inspection, not automatic retry.",
            "parameters":{"type":"object","properties":save,"required":["operation","target","fileVersion","expectedActivation","definition","reason"],"additionalProperties":false},
            "effects":{"reads":["*"],"writes":["*"],"network":["*"],"executes":true,"admits":"reusable-expert-definition","reversible":false}}),
        json!({"name":DELETE,"description":"Remove an inspected project or personal expert definition after explicit human confirmation. Copy deleteArguments from InspectExpert and add a reason. Accepted jobs keep their snapshots and history remains intact. Identical recreation requires activation again. Built-ins are read-only.",
            "parameters":{"type":"object","properties":delete,"required":["operation","target","fileVersion","expectedActivation","reason"],"additionalProperties":false},
            "effects":{"reads":["expert definitions and activation state"],"writes":["expert definitions and activation state"],"network":[],"executes":false,"reversible":false}}),
    ]
}
