use super::*;
use crate::contracts::component::PortDecl;

struct Idle;

impl Component for Idle {
    fn handle(&mut self, _: &str, _: &EventEnvelope, _: &mut Ctx) {}
}

// Keep the real dispatcher and mailbox, but acknowledge deliveries explicitly.
// No worker scheduling or elapsed wall time determines the queue's state.
fn controlled_mailbox(workers: usize) -> (Kernel, mpsc::Receiver<Delivery>) {
    let manifest = ComponentManifest {
        name: "receiver".into(),
        version: "0.0.0".into(),
        runtime: RuntimeKind::Inproc,
        entry: "builtin:receiver".into(),
        inputs: vec![PortDecl::new(
            "in",
            &[ce::TOOL_EXEC_STARTED, ce::INTERRUPTED],
        )],
        outputs: vec![PortDecl::new("out", &[ce::TOOL_EXEC_STARTED])],
        events: vec![],
        default_wiring: vec![],
        capabilities: None,
        implements: vec![],
        tools: vec![],
        prompt: None,
        handle_timeout_ms: Some(60_000),
        concurrency: Some(workers),
    };
    let assembly = AssemblyManifest {
        instances: [(
            "receiver".into(),
            ComponentInstance {
                component: "receiver".into(),
                requires: vec![],
                config: None,
            },
        )]
        .into(),
        wires: vec![Wire::new("receiver.out", "receiver.in")],
    };
    let mut factories: HashMap<String, Factory> = HashMap::new();
    factories.insert("receiver".into(), Box::new(|_| Box::new(Idle)));
    let mut kernel = Kernel::start(
        &assembly,
        &[("receiver".into(), manifest)].into(),
        &mut factories,
        KernelOptions::default(),
    )
    .unwrap();
    let (mailbox, receiver) = mpsc::channel();
    kernel.mailboxes.insert("receiver".into(), mailbox);
    (kernel, receiver)
}

fn request(kernel: &mut Kernel, receiver: &mpsc::Receiver<Delivery>, call: &str) -> Delivery {
    kernel.dispatch(
        "receiver".into(),
        "out".into(),
        EventDraft::new(
            ce::TOOL_EXEC_STARTED,
            &[],
            json!({"call": call, "tool": "probe", "arguments": {}}),
        ),
    );
    receiver.try_recv().expect("dispatch delivered the request")
}

#[test]
fn settlement_deadline_starts_on_promotion_not_while_queued() {
    for workers in [1, 2] {
        let (mut kernel, receiver) = controlled_mailbox(workers);
        let mut active = Vec::new();
        for seat in 0..workers {
            active.push(request(&mut kernel, &receiver, &format!("active-{seat}")));
        }
        let (_, queued, _) = request(&mut kernel, &receiver, "queued");
        assert!(kernel.outstanding["receiver"]
            .back()
            .unwrap()
            .deadline
            .is_none());
        kernel.settle_chain(&active[0].1.id, "test", "test settlement", json!({}));
        let (_, ending, token) = receiver.try_recv().expect("settlement was delivered");
        assert_eq!(ending.event_type, ce::INTERRUPTED);
        assert_eq!(kernel.in_flight, workers + 2);
        assert!(
            kernel.outstanding["receiver"]
                .back()
                .unwrap()
                .deadline
                .is_none(),
            "a queued settlement must not spend its handling budget before promotion"
        );

        // Finish enough deliveries to promote the ending. For two workers,
        // finish the second worker first to exercise out-of-order completion.
        active.reverse();
        let promotion_started = Instant::now();
        let mut dispatched = 0;
        for (_, event, _) in active {
            kernel.process(
                Message::Processed {
                    instance: "receiver".into(),
                    event: event.id,
                },
                &mut dispatched,
            );
        }
        if workers == 1 {
            assert!(kernel.outstanding["receiver"]
                .back()
                .unwrap()
                .deadline
                .is_none());
            kernel.process(
                Message::Processed {
                    instance: "receiver".into(),
                    event: queued.id,
                },
                &mut dispatched,
            );
        }
        let promotion_finished = Instant::now();
        let ending_delivery = kernel
            .outstanding
            .get_mut("receiver")
            .unwrap()
            .iter_mut()
            .find(|delivery| delivery.event_id == ending.id)
            .unwrap();
        let deadline = ending_delivery
            .deadline
            .expect("promotion must start the handling clock");
        let budget = Duration::from_secs(60);
        assert!(deadline >= promotion_started + budget);
        assert!(deadline <= promotion_finished + budget);
        assert!(!token.is_cancelled());
        // Expire that clock directly, without sleeping. The recipient and
        // bookkeeping must still hold the same cancellation token.
        ending_delivery.deadline = Some(Instant::now());
        kernel.handle_expiry("receiver", &ending.id);
        assert!(token.is_cancelled());
        assert!(kernel.outstanding["receiver"]
            .iter()
            .find(|delivery| delivery.event_id == ending.id)
            .unwrap()
            .grace_deadline
            .is_some());
        kernel.shutdown();
    }
}

#[test]
fn settlement_in_an_available_worker_slot_has_a_deadline_immediately() {
    let (mut kernel, receiver) = controlled_mailbox(2);
    let (_, call, _) = request(&mut kernel, &receiver, "active");
    kernel.settle_chain(&call.id, "test", "test settlement", json!({}));
    let (_, ending, _) = receiver.try_recv().expect("settlement was delivered");
    let delivery = kernel.outstanding["receiver"].back().unwrap();
    assert_eq!(delivery.event_id, ending.id);
    assert!(delivery.deadline.is_some());
    kernel.shutdown();
}
