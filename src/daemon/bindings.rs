//! Host-owned attachment lifetimes. Permission decisions remain in components.
use super::protocol::{
    AttachmentHistory, AuthorizationAttachment, ServerMessage, PERMISSIONS_CAPABILITY,
};
use crate::{
    components::{interface_permissions as ip, operation_policy as op},
    core_events as ce, EventDraft, Injector, Kernel, LogReader,
};
use serde_json::json;
use std::collections::HashMap;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};

#[cfg(test)]
#[path = "bindings_tests.rs"]
mod tests;

type Outbox = std::sync::mpsc::Sender<ServerMessage>;
struct Attachment {
    token: String,
    interface: Option<String>,
    negotiated: bool,
    active: bool,
    alive: Arc<AtomicBool>,
    outbox: Outbox,
}

pub(super) struct PreparedAttachment {
    pub stream: String,
    pub replay: Vec<crate::EventEnvelope>,
    pub warnings: Vec<String>,
    pub history: Option<AttachmentHistory>,
    pub through: u64,
    pub pending_auth: Vec<super::protocol::PendingAuthorization>,
}

pub(super) struct Bindings {
    clients: Mutex<HashMap<u64, Attachment>>,
    injector: Injector,
    reader: LogReader,
    interface_service: Option<String>,
    operation_service: Option<String>,
}

impl Bindings {
    pub fn new(kernel: &Kernel) -> Self {
        let service = |kind: &str| {
            kernel.assembly().wires.iter().find_map(|wire| {
                if wire.from != "ui.answer" {
                    return None;
                }
                let (id, _) = wire.to.rsplit_once('.')?;
                let spec = kernel.assembly().instances.get(id)?;
                let manifest = kernel.component_registry().get(&spec.component)?;
                manifest
                    .outputs
                    .iter()
                    .any(|port| port.events.iter().any(|event| event == kind))
                    .then(|| id.to_owned())
            })
        };
        Self {
            clients: Mutex::new(HashMap::new()),
            injector: kernel.injector("ui"),
            reader: kernel.log().reader(),
            interface_service: service(ip::STATE),
            operation_service: service(op::STATE),
        }
    }

    fn close(&self, binding: Attachment) {
        if binding.active {
            if let Some(id) = binding.interface {
                self.injector.emit(
                    "answer",
                    EventDraft::new(
                        ce::EXTERNAL_INPUT,
                        &[],
                        json!({"channel":ip::CHANNEL,"interface":id,"action":"close"}),
                    ),
                );
            }
        }
    }

    pub fn prepare(
        &self,
        client: u64,
        outbox: Outbox,
        capabilities: &[String],
        alive: Arc<AtomicBool>,
    ) -> String {
        let mut clients = self.clients.lock().unwrap();
        if let Some(previous) = clients.remove(&client) {
            self.close(previous);
        }
        let token = ip::new_instance_id(&self.reader);
        clients.insert(
            client,
            Attachment {
                interface: self.interface_service.as_ref().map(|_| token.clone()),
                token: token.clone(),
                negotiated: capabilities.iter().any(|c| c == PERMISSIONS_CAPABILITY),
                active: false,
                alive,
                outbox,
            },
        );
        token
    }

    pub fn current(&self, client: u64, token: &str) -> bool {
        self.clients
            .lock()
            .unwrap()
            .get(&client)
            .is_some_and(|b| b.token == token && b.alive.load(Ordering::Acquire))
    }

    pub fn active(&self, client: u64) -> bool {
        self.clients
            .lock()
            .unwrap()
            .get(&client)
            .is_some_and(|b| b.active && b.alive.load(Ordering::Acquire))
    }

    pub fn source(&self, client: u64) -> Option<String> {
        self.clients
            .lock()
            .unwrap()
            .get(&client)
            .filter(|b| b.active && b.alive.load(Ordering::Acquire))
            .and_then(|b| b.interface.clone())
    }

    /// Recheck the generation under the same lock used by detach. A queued
    /// subscribe cannot resurrect a connection that ended during history I/O.
    pub fn attach(
        &self,
        client: u64,
        token: &str,
        prepared: PreparedAttachment,
    ) -> Result<(), String> {
        let PreparedAttachment {
            stream,
            replay,
            warnings,
            history,
            through,
            pending_auth,
        } = prepared;
        let mut clients = self.clients.lock().unwrap();
        let Some(binding) = clients
            .get_mut(&client)
            .filter(|b| b.token == token && b.alive.load(Ordering::Acquire))
        else {
            return Ok(());
        };
        let message = if binding.negotiated {
            let grants = match &self.operation_service {
                Some(source) => op::read_grants(&self.reader, source).map_err(|e| e.to_string())?,
                None => op::GrantState::default(),
            };
            ServerMessage::AttachedV2 {
                stream,
                replay,
                warnings,
                history,
                authorization: AuthorizationAttachment {
                    attachment: binding.token.clone(),
                    interface: binding.interface.clone(),
                    interface_service: self.interface_service.clone(),
                    operation_service: self.operation_service.clone(),
                    through,
                    grants,
                    pending_authorizations: pending_auth
                        .iter()
                        .map(|card| {
                            let event = self
                                .reader
                                .get(&card.request)
                                .map_err(|e| e.to_string())?
                                .ok_or_else(|| {
                                    format!("missing authorization question {}", card.request)
                                })?;
                            if event.seq > through {
                                return Err(
                                    "authorization question lies beyond attachment prefix".into()
                                );
                            }
                            Ok(event)
                        })
                        .collect::<Result<_, String>>()?,
                },
            }
        } else {
            ServerMessage::Attached {
                stream,
                replay,
                warnings,
                history,
            }
        };
        if let Some(id) = &binding.interface {
            self.injector.emit(
                "answer",
                EventDraft::new(
                    ce::EXTERNAL_INPUT,
                    &[],
                    json!({"channel":ip::CHANNEL,"interface":id,"action":"open"}),
                ),
            );
        }
        binding.active = true;
        if binding.outbox.send(message).is_err() {
            self.close(clients.remove(&client).unwrap());
        }
        Ok(())
    }

    pub fn detach(&self, client: u64) {
        if let Some(binding) = self.clients.lock().unwrap().remove(&client) {
            self.close(binding);
        }
    }

    pub fn close_all(&self) {
        for (_, binding) in self.clients.lock().unwrap().drain() {
            self.close(binding);
        }
    }

    pub fn broadcast(&self, message: ServerMessage) {
        let mut clients = self.clients.lock().unwrap();
        let dead: Vec<_> = clients
            .iter()
            .filter_map(|(id, b)| {
                (b.active
                    && (!b.alive.load(Ordering::Acquire)
                        || b.outbox.send(message.clone()).is_err()))
                .then_some(*id)
            })
            .collect();
        for id in dead {
            self.close(clients.remove(&id).unwrap());
        }
    }

    pub fn control(
        &self,
        client: u64,
        token: &str,
        mut payload: serde_json::Value,
        interface_control: bool,
    ) -> Result<(), String> {
        let clients = self.clients.lock().unwrap();
        let binding = clients
            .get(&client)
            .filter(|b| {
                b.active && b.token == token && b.negotiated && b.alive.load(Ordering::Acquire)
            })
            .ok_or("stale, unnegotiated, or inactive attachment")?;
        if if interface_control {
            self.interface_service.is_none()
        } else {
            self.operation_service.is_none()
        } {
            return Err(
                "this assembly does not support the requested authorization control".into(),
            );
        }
        payload["interface"] = json!(binding.interface);
        // Serialize against close, not against the model's execution queue.
        self.injector
            .emit("answer", EventDraft::new(ce::EXTERNAL_INPUT, &[], payload));
        Ok(())
    }
}
