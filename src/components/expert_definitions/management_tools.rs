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
        json!({"name":LIST,"description":"List built-in, project and personal experts, readiness and model choices. toolRoot is the configured file-tool root (null means unconfined); projectRoot and personalRoot locate stored definitions, not extra filesystem access. Use InspectExpert for the current snapshot.",
            "parameters":{"type":"object","properties":{},"additionalProperties":false},
            "effects":{"reads":["expert definitions, model catalog and authorization evidence"],"writes":["expert state locks and process credential references"],"network":[],"executes":false}}),
        json!({"name":SAVE,"description":"Create, update or copy an expert. Use the destination snapshot from InspectExpert or the latest save/activation details: copy target and fileVersion, copy activation as expectedActivation, add operation=put, the proposed definition and a reason. fileVersion=null expects absence. Copies never inherit activation. Returns details of the saved revision; activate explicitly to make it available. Conflicts and partial outcomes require fresh inspection, not automatic retry.",
            "parameters":{"type":"object","properties":save,"required":["operation","target","fileVersion","expectedActivation","definition","reason"],"additionalProperties":false},
            "effects":{"reads":["*"],"writes":["*"],"network":["*"],"executes":true,"admits":"reusable-expert-definition","reversible":false}}),
        json!({"name":DELETE,"description":"Remove a custom expert after explicit human confirmation. From InspectExpert or the latest save/activation details, copy target and fileVersion, copy activation as expectedActivation, and add operation=delete and a reason. Accepted jobs and history remain intact. Identical recreation requires activation again. Built-ins are read-only.",
            "parameters":{"type":"object","properties":delete,"required":["operation","target","fileVersion","expectedActivation","reason"],"additionalProperties":false},
            "effects":{"reads":["expert definitions and activation state"],"writes":["expert definitions and activation state"],"network":[],"executes":false,"reversible":false}}),
    ]
}
