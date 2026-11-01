use super::*;
use crate::appliance_updates::*;
use crate::sync_control::{
    FolderControlOperation as Op, FolderControlPayload as Payload, send_folder_control,
};
use covalent_core::appliance_import::ImportApprovalBond;
use covalent_core::appliance_updates::*;
use sha2::{Digest as _, Sha256};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

// In-memory privileged-host double. Durable effects are qualified by the Pi suite.
struct Host {
    context: Mutex<AutomaticUpdateContext>,
    last: Mutex<Option<(SignedAutomaticUpdateChange, AutomaticUpdateReceipt)>>,
    calls: AtomicUsize,
    effects: AtomicUsize,
    lose_reply: AtomicBool,
    forged_receipt: AtomicBool,
    hold: AtomicBool,
    entered: tokio::sync::Notify,
    release: tokio::sync::Notify,
}
impl Host {
    fn new(pi: &Device, phone: &Device, generation: Uuid) -> Self {
        Self {
            context: Mutex::new(AutomaticUpdateContext {
                schema_version: 1,
                bond: ImportApprovalBond {
                    appliance: pi.engine.public_identity(),
                    controller: phone.engine.public_identity(),
                    root_generation: Uuid::new_v4(),
                    native_generation: generation,
                },
                configured: true,
                enabled: false,
                revision: Some(Uuid::new_v4()),
                busy: false,
            }),
            last: Mutex::new(None),
            calls: AtomicUsize::new(0),
            effects: AtomicUsize::new(0),
            lose_reply: AtomicBool::new(false),
            forged_receipt: AtomicBool::new(false),
            hold: AtomicBool::new(false),
            entered: tokio::sync::Notify::new(),
            release: tokio::sync::Notify::new(),
        }
    }
    fn proof(&self, phone: &Device, enabled: bool) -> SignedAutomaticUpdateChange {
        let c = self.context.lock().unwrap();
        sign(
            phone,
            &AutomaticUpdateChange {
                schema_version: 1,
                bond: c.bond.clone(),
                action_id: Uuid::new_v4(),
                expected_revision: c.revision.unwrap(),
                enabled,
            },
        )
    }
}
fn sign(phone: &Device, choice: &AutomaticUpdateChange) -> SignedAutomaticUpdateChange {
    let bytes = choice.canonical_bytes().unwrap();
    SignedAutomaticUpdateChange {
        signature: phone
            .engine
            .sign_transport_transcript_with_domain(AUTOMATIC_UPDATES_DOMAIN, &bytes),
        payload: String::from_utf8(bytes).unwrap(),
    }
}
impl ApplianceUpdateHost for Host {
    fn context(&self) -> UpdateHostFuture<'_, AutomaticUpdateContext> {
        Box::pin(async {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(self.context.lock().unwrap().clone())
        })
    }
    fn action<'a>(
        &'a self,
        operation: UpdateOperation,
        proof: &'a SignedAutomaticUpdateChange,
    ) -> UpdateHostFuture<'a, AutomaticUpdateReceipt> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.hold.swap(false, Ordering::SeqCst) {
                self.entered.notify_one();
                self.release.notified().await;
            }
            let mut c = self.context.lock().unwrap();
            let mut last = self.last.lock().unwrap();
            let previous = last
                .as_ref()
                .filter(|(p, _)| p == proof)
                .map(|(p, _)| {
                    serde_json::from_str::<AutomaticUpdateChange>(&p.payload)
                        .unwrap()
                        .expected_revision
                })
                .unwrap_or(c.revision.unwrap());
            let choice = verify_automatic_update_change(&c.bond, previous, proof)
                .map_err(|_| UpdateHostError)?;
            if let Some((saved, r)) = last.as_ref()
                && saved == proof
            {
                return Ok(r.clone());
            }
            let mut receipt = AutomaticUpdateReceipt {
                schema_version: 1,
                state: UpdateOutcome::NotObserved,
                action_id: choice.action_id,
                payload_digest: format!("{:x}", Sha256::digest(proof.payload.as_bytes())),
                revision: c.revision.unwrap(),
                enabled: c.enabled,
            };
            if operation == UpdateOperation::Apply {
                self.effects.fetch_add(1, Ordering::SeqCst);
                c.enabled = choice.enabled;
                c.revision = Some(Uuid::new_v4());
                receipt.state = UpdateOutcome::Applied;
                receipt.enabled = c.enabled;
                receipt.revision = c.revision.unwrap();
                *last = Some((proof.clone(), receipt.clone()));
            }
            if self.lose_reply.swap(false, Ordering::SeqCst) {
                return Err(UpdateHostError);
            }
            if self.forged_receipt.swap(false, Ordering::SeqCst) {
                receipt.action_id = Uuid::new_v4();
            }
            Ok(receipt)
        })
    }
}
fn read(phone: &Device, pi: &Device) -> Op {
    Op::ReadApplianceUpdates {
        requester_id: phone.engine.device_id(),
        target_id: pi.engine.device_id(),
    }
}
fn action(
    phone: &Device,
    pi: &Device,
    operation: UpdateOperation,
    proof: &SignedAutomaticUpdateChange,
) -> Op {
    Op::ApplianceUpdate {
        requester_id: phone.engine.device_id(),
        target_id: pi.engine.device_id(),
        operation,
        proof: proof.clone(),
    }
}

#[tokio::test]
async fn paired_peers_need_owner_approval_and_only_exact_signed_choices_reach_the_host() {
    let pi = Device::new();
    let phone = Device::new();
    let other = Device::new();
    let state = pi.state();
    let store = state.controller_bond.as_ref().unwrap().clone();
    let host = Arc::new(Host::new(&pi, &phone, store.snapshot().unwrap().generation));
    let state = state.with_appliance_updates(host.clone());
    let node = QuicNode::bind(([127, 0, 0, 1], 0).into(), pi.engine.clone(), &pi.tls).unwrap();
    let address = node.local_addr().unwrap();
    eprintln!("owned update-policy QUIC fixture {address}");
    let binding = pair(&phone, &pi, address);
    let other_binding = pair(&other, &pi, address);
    let shutdown = node.shutdown_handle();
    let server = tokio::spawn(
        node.with_local_provider_enabled(false)
            .with_controller_bond(state.controller_bond.clone())
            .run(),
    );
    assert_eq!(
        send_folder_control(phone.engine.clone(), binding.clone(), read(&phone, &pi))
            .await
            .unwrap(),
        Payload::Rejected
    );
    let proof = host.proof(&phone, true);
    assert_eq!(
        send_folder_control(
            phone.engine.clone(),
            binding.clone(),
            action(&phone, &pi, UpdateOperation::Apply, &proof)
        )
        .await
        .unwrap(),
        Payload::Rejected
    );
    assert_eq!(host.calls.load(Ordering::SeqCst), 0);
    let bond = store
        .grant(
            &pi.engine,
            store.snapshot().unwrap().generation,
            phone.engine.public_identity(),
        )
        .unwrap();
    host.context.lock().unwrap().bond.native_generation = bond.generation;
    let Payload::ApplianceUpdates(status) =
        send_folder_control(phone.engine.clone(), binding.clone(), read(&phone, &pi))
            .await
            .unwrap()
    else {
        panic!("missing update context")
    };
    assert!(status.updates_supported && status.updates_available);
    assert_eq!(status.context.unwrap(), *host.context.lock().unwrap());
    let calls = host.calls.load(Ordering::SeqCst);
    assert_eq!(
        send_folder_control(other.engine.clone(), other_binding, read(&other, &pi))
            .await
            .unwrap(),
        Payload::Rejected
    );
    let proof = host.proof(&phone, true);
    let choice: AutomaticUpdateChange = serde_json::from_str(&proof.payload).unwrap();
    let mut changed = choice.clone();
    changed.bond.native_generation = Uuid::new_v4();
    let mut wrong_key = choice.clone();
    wrong_key.bond.controller = other.engine.public_identity();
    let wrong_domain = SignedAutomaticUpdateChange {
        payload: proof.payload.clone(),
        signature: phone.engine.sign_transport_transcript_with_domain(
            covalent_core::appliance_import::IMPORT_APPROVAL_DOMAIN,
            proof.payload.as_bytes(),
        ),
    };
    for bad in [
        SignedAutomaticUpdateChange {
            payload: proof
                .payload
                .replace("\"enabled\":true", "\"enabled\":false"),
            ..proof.clone()
        },
        sign(&phone, &changed),
        sign(&other, &wrong_key),
        wrong_domain,
    ] {
        assert_eq!(
            send_folder_control(
                phone.engine.clone(),
                binding.clone(),
                action(&phone, &pi, UpdateOperation::Apply, &bad)
            )
            .await
            .unwrap(),
            Payload::Rejected
        );
    }
    assert_eq!(host.calls.load(Ordering::SeqCst), calls);
    let mut root_changed = choice.clone();
    root_changed.bond.root_generation = Uuid::new_v4();
    assert_eq!(
        send_folder_control(
            phone.engine.clone(),
            binding.clone(),
            action(
                &phone,
                &pi,
                UpdateOperation::Apply,
                &sign(&phone, &root_changed)
            )
        )
        .await
        .unwrap(),
        Payload::NeedsAttention
    );
    assert_eq!(host.effects.load(Ordering::SeqCst), 0);
    let first = send_folder_control(
        phone.engine.clone(),
        binding.clone(),
        action(&phone, &pi, UpdateOperation::Apply, &proof),
    )
    .await
    .unwrap();
    assert!(
        matches!(&first,Payload::ApplianceUpdateReceipt(r) if r.state==UpdateOutcome::Applied && r.enabled)
    );
    assert_eq!(
        send_folder_control(
            phone.engine.clone(),
            binding.clone(),
            action(&phone, &pi, UpdateOperation::Apply, &proof)
        )
        .await
        .unwrap(),
        first
    );
    assert_eq!(
        send_folder_control(
            phone.engine.clone(),
            binding.clone(),
            action(&phone, &pi, UpdateOperation::Lookup, &proof)
        )
        .await
        .unwrap(),
        first
    );
    assert_eq!(host.effects.load(Ordering::SeqCst), 1);
    let next = host.proof(&phone, false);
    host.lose_reply.store(true, Ordering::SeqCst);
    assert_eq!(
        send_folder_control(
            phone.engine.clone(),
            binding.clone(),
            action(&phone, &pi, UpdateOperation::Apply, &next)
        )
        .await
        .unwrap(),
        Payload::NeedsAttention
    );
    let recovered = send_folder_control(
        phone.engine.clone(),
        binding.clone(),
        action(&phone, &pi, UpdateOperation::Lookup, &next),
    )
    .await
    .unwrap();
    assert!(
        matches!(recovered,Payload::ApplianceUpdateReceipt(r) if r.state==UpdateOutcome::Applied && !r.enabled)
    );
    assert_eq!(host.effects.load(Ordering::SeqCst), 2);
    assert_eq!(
        send_folder_control(
            phone.engine.clone(),
            binding.clone(),
            action(&phone, &pi, UpdateOperation::Apply, &proof)
        )
        .await
        .unwrap(),
        Payload::NeedsAttention
    );
    let never_sent = host.proof(&phone, true);
    host.forged_receipt.store(true, Ordering::SeqCst);
    assert_eq!(
        send_folder_control(
            phone.engine.clone(),
            binding.clone(),
            action(&phone, &pi, UpdateOperation::Lookup, &never_sent)
        )
        .await
        .unwrap(),
        Payload::Rejected
    );
    let unknown = send_folder_control(
        phone.engine.clone(),
        binding.clone(),
        action(&phone, &pi, UpdateOperation::Lookup, &never_sent),
    )
    .await
    .unwrap();
    assert!(
        matches!(unknown,Payload::ApplianceUpdateReceipt(r) if r.state==UpdateOutcome::NotObserved && !r.enabled)
    );
    assert_eq!(host.effects.load(Ordering::SeqCst), 2);
    let _held = store.host_ops.clone().acquire_owned().await.unwrap();
    assert_eq!(
        send_folder_control(phone.engine.clone(), binding.clone(), read(&phone, &pi))
            .await
            .unwrap(),
        Payload::Busy
    );
    drop(_held);
    store.revoke(bond.generation).unwrap();
    let calls = host.calls.load(Ordering::SeqCst);
    assert_eq!(
        send_folder_control(
            phone.engine.clone(),
            binding.clone(),
            action(&phone, &pi, UpdateOperation::Lookup, &next)
        )
        .await
        .unwrap(),
        Payload::Rejected
    );
    assert_eq!(host.calls.load(Ordering::SeqCst), calls);
    shutdown.close();
    server.await.unwrap();
    shutdown.wait_for_release().await.unwrap();
    assert!(std::net::UdpSocket::bind(address).is_ok());
}

#[tokio::test]
async fn changed_owner_during_host_call_cannot_receive_a_success_receipt() {
    let pi = Device::new();
    let phone = Device::new();
    let state = pi.state();
    let node = QuicNode::bind(([127, 0, 0, 1], 0).into(), pi.engine.clone(), &pi.tls).unwrap();
    let address = node.local_addr().unwrap();
    eprintln!("owned revoked-update QUIC fixture {address}");
    let binding = pair(&phone, &pi, address);
    let store = state.controller_bond.as_ref().unwrap().clone();
    let bond = store
        .grant(
            &pi.engine,
            store.snapshot().unwrap().generation,
            phone.engine.public_identity(),
        )
        .unwrap();
    let host = Arc::new(Host::new(&pi, &phone, bond.generation));
    let state = state.with_appliance_updates(host.clone());
    let shutdown = node.shutdown_handle();
    let server = tokio::spawn(
        node.with_local_provider_enabled(false)
            .with_controller_bond(state.controller_bond.clone())
            .run(),
    );
    let proof = host.proof(&phone, true);
    host.hold.store(true, Ordering::SeqCst);
    let op = action(&phone, &pi, UpdateOperation::Lookup, &proof);
    let engine = phone.engine.clone();
    let request = tokio::spawn(async move { send_folder_control(engine, binding, op).await });
    tokio::time::timeout(std::time::Duration::from_secs(5), host.entered.notified())
        .await
        .unwrap();
    store.revoke(bond.generation).unwrap();
    host.release.notify_one();
    assert_eq!(request.await.unwrap().unwrap(), Payload::Rejected);
    assert_eq!(host.effects.load(Ordering::SeqCst), 0);
    shutdown.close();
    server.await.unwrap();
    shutdown.wait_for_release().await.unwrap();
    assert!(std::net::UdpSocket::bind(address).is_ok());
}

#[tokio::test]
async fn absent_or_invalid_host_never_claims_a_configured_update_policy() {
    let pi = Device::new();
    let phone = Device::new();
    let state = pi.state();
    pair(&phone, &pi, ([127, 0, 0, 1], 43124).into());
    let store = state.controller_bond.as_ref().unwrap().clone();
    let bond = store
        .grant(
            &pi.engine,
            store.snapshot().unwrap().generation,
            phone.engine.public_identity(),
        )
        .unwrap();
    let absent = store
        .read_updates(&pi.engine, phone.engine.device_id())
        .await
        .unwrap();
    assert!(!absent.updates_supported && !absent.updates_available && absent.context.is_none());
    let host = Arc::new(Host::new(&pi, &phone, bond.generation));
    let _state = state.with_appliance_updates(host.clone());
    let base = host.context.lock().unwrap().clone();
    for bad in [
        AutomaticUpdateContext {
            schema_version: 2,
            ..base.clone()
        },
        AutomaticUpdateContext {
            bond: ImportApprovalBond {
                appliance: phone.engine.public_identity(),
                ..base.bond.clone()
            },
            ..base.clone()
        },
        AutomaticUpdateContext {
            bond: ImportApprovalBond {
                native_generation: Uuid::new_v4(),
                ..base.bond.clone()
            },
            ..base.clone()
        },
        AutomaticUpdateContext {
            revision: None,
            ..base.clone()
        },
    ] {
        *host.context.lock().unwrap() = bad;
        let status = store
            .read_updates(&pi.engine, phone.engine.device_id())
            .await
            .unwrap();
        assert!(status.updates_supported && !status.updates_available && status.context.is_none());
    }
    *host.context.lock().unwrap() = AutomaticUpdateContext {
        configured: false,
        enabled: false,
        revision: None,
        ..base
    };
    let status = store
        .read_updates(&pi.engine, phone.engine.device_id())
        .await
        .unwrap();
    assert!(
        status.updates_supported && status.updates_available && !status.context.unwrap().configured
    );
}
