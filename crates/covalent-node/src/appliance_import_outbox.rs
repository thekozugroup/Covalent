//! Persist before the first send; uncertain commands are checked, never replayed.
use crate::appliance_imports::{ImportOutcome, ImportReceipt, ImportSource};
use covalent_core::{
    PublicIdentity,
    appliance_import::{ImportApproval, SignedImportApproval, verify_import_approval},
};
use serde::{Deserialize, Serialize};
use std::{fs::File, io::Read as _, path::PathBuf, sync::Mutex};
use uuid::Uuid;

const MAX_BYTES: u64 = 128 * 1024;
const MAX_RECORDS: usize = 32;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ImportAction {
    pub order: u64,
    pub proof: SignedImportApproval,
    pub source: ImportSource,
    pub withdrawal_requested: bool,
    #[serde(deserialize_with = "nullable")]
    pub withdrawal_signature: Option<String>,
    #[serde(deserialize_with = "nullable")]
    pub superseded_by: Option<Uuid>,
    #[serde(deserialize_with = "nullable")]
    pub receipt: Option<ImportReceipt>,
}
fn nullable<'de, D: serde::Deserializer<'de>, T: Deserialize<'de>>(
    d: D,
) -> Result<Option<T>, D::Error> {
    Option::<T>::deserialize(d)
}
impl ImportAction {
    pub(crate) fn choice(&self) -> Result<ImportApproval, ()> {
        let choice: ImportApproval = serde_json::from_str(&self.proof.payload).map_err(|_| ())?;
        verify_import_approval(&choice.bond, &self.proof).map_err(|_| ())
    }
    pub fn resolved(&self) -> bool {
        self.superseded_by.is_some()
            || self.receipt.as_ref().is_some_and(|r| {
                matches!(
                    r.outcome,
                    ImportOutcome::Accepted | ImportOutcome::Expired | ImportOutcome::Withdrawn
                )
            })
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Saved {
    schema_version: u16,
    controller: PublicIdentity,
    order: u64,
    actions: Vec<ImportAction>,
}

pub(crate) struct ImportOutbox {
    path: PathBuf,
    _lock: File,
    state: Mutex<Option<Saved>>,
}
impl ImportOutbox {
    pub(crate) fn open(path: PathBuf, controller: PublicIdentity) -> Option<Self> {
        use rustix::fs::{FileType, Mode, OFlags, fstat, open};
        let parent = path.parent()?;
        if parent.canonicalize().ok()?.as_path() != parent {
            return None;
        }
        let metadata = std::fs::symlink_metadata(parent).ok()?;
        use std::os::unix::fs::MetadataExt as _;
        if !metadata.is_dir()
            || metadata.uid() != rustix::process::geteuid().as_raw()
            || metadata.mode() & 0o077 != 0
        {
            return None;
        }
        let lock_path = path.with_extension("lock");
        let (fd, fresh) = match open(
            &lock_path,
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::RUSR | Mode::WUSR,
        ) {
            Ok(fd) => (fd, true),
            Err(rustix::io::Errno::EXIST) => (
                open(
                    &lock_path,
                    OFlags::WRONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
                    Mode::empty(),
                )
                .ok()?,
                false,
            ),
            Err(_) => return None,
        };
        let stat = fstat(&fd).ok()?;
        if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile
            || stat.st_uid != rustix::process::geteuid().as_raw()
            || stat.st_nlink != 1
            || stat.st_size != 0
            || stat.st_mode & 0o077 != 0
        {
            return None;
        }
        let lock = File::from(fd);
        fs2::FileExt::try_lock_exclusive(&lock).ok()?;
        let saved = match open(
            &path,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        ) {
            Err(rustix::io::Errno::NOENT) if fresh => Saved {
                schema_version: 1,
                controller: controller.clone(),
                order: 0,
                actions: Vec::new(),
            },
            Err(_) => return None,
            Ok(fd) => {
                let st = fstat(&fd).ok()?;
                if FileType::from_raw_mode(st.st_mode) != FileType::RegularFile
                    || st.st_uid != rustix::process::geteuid().as_raw()
                    || st.st_nlink != 1
                    || st.st_mode & 0o077 != 0
                    || st.st_size < 0
                    || st.st_size as u64 > MAX_BYTES
                {
                    return None;
                }
                let mut bytes = Vec::new();
                File::from(fd)
                    .take(MAX_BYTES + 1)
                    .read_to_end(&mut bytes)
                    .ok()?;
                if bytes.len() as u64 > MAX_BYTES {
                    return None;
                }
                serde_json::from_slice::<Saved>(&bytes).ok()?
            }
        };
        if saved.schema_version != 1
            || saved.controller != controller
            || saved.actions.len() > MAX_RECORDS
        {
            return None;
        }
        let mut ids = std::collections::BTreeSet::new();
        let mut orders = std::collections::BTreeSet::new();
        for a in &saved.actions {
            let c = a.choice().ok()?;
            if c.bond.controller != controller
                || !ids.insert(c.action_id)
                || a.order == 0
                || a.order > saved.order
                || !orders.insert(a.order)
                || a.source.source_id != c.source_id
                || a.source.kind != c.kind
                || a.source.action_session != c.action_session
                || a.source.action_sequence != c.action_sequence
                || a.withdrawal_requested != a.withdrawal_signature.is_some()
                || a.withdrawal_signature.as_ref().is_some_and(|s| {
                    covalent_core::appliance_import::verify_import_withdrawal(&c.bond, &a.proof, s)
                        .is_err()
                })
                || a.receipt
                    .as_ref()
                    .is_some_and(|r| r.validate_for(&a.proof).is_err())
            {
                return None;
            }
            if let Some(id) = a.superseded_by {
                let later = saved
                    .actions
                    .iter()
                    .find(|p| p.choice().is_ok_and(|c| c.action_id == id))?;
                let p = later.choice().ok()?;
                if later.order <= a.order
                    || p.mode != covalent_core::appliance_import::ImportApprovalMode::Off
                    || p.bond.appliance != c.bond.appliance
                    || p.source_id != c.source_id
                    || !later
                        .receipt
                        .as_ref()
                        .is_some_and(|r| r.outcome == ImportOutcome::Accepted)
                {
                    return None;
                }
            }
        }
        let value = Self {
            path,
            _lock: lock,
            state: Mutex::new(Some(saved.clone())),
        };
        if fresh {
            let mut guard = value.state.lock().ok()?;
            value.persist(&mut guard, saved).ok()?;
        }
        Some(value)
    }
    fn persist(&self, current: &mut Option<Saved>, next: Saved) -> Result<(), ()> {
        let bytes = serde_json::to_vec(&next).map_err(|_| ())?;
        if bytes.len() as u64 > MAX_BYTES {
            return Err(());
        }
        if crate::persist_private_file(&self.path, &bytes).is_err() {
            *current = None;
            return Err(());
        }
        *current = Some(next);
        Ok(())
    }
    fn ready(&self, current: &mut Option<Saved>) -> Result<(), ()> {
        use rustix::fs::{FileType, Mode, OFlags, fstat, open};
        let checked = (|| {
            let expected = serde_json::to_vec(current.as_ref().ok_or(())?).map_err(|_| ())?;
            let parent = self.path.parent().ok_or(())?;
            if parent.canonicalize().map_err(|_| ())?.as_path() != parent {
                return Err(());
            }
            let fd = open(
                &self.path,
                OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map_err(|_| ())?;
            let st = fstat(&fd).map_err(|_| ())?;
            if FileType::from_raw_mode(st.st_mode) != FileType::RegularFile
                || st.st_uid != rustix::process::geteuid().as_raw()
                || st.st_nlink != 1
                || st.st_mode & 0o077 != 0
                || st.st_size < 0
                || st.st_size as u64 > MAX_BYTES
            {
                return Err(());
            }
            let mut bytes = Vec::new();
            File::from(fd)
                .take(MAX_BYTES + 1)
                .read_to_end(&mut bytes)
                .map_err(|_| ())?;
            let lock = open(
                self.path.with_extension("lock"),
                OFlags::WRONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map_err(|_| ())?;
            let old = fstat(&self._lock).map_err(|_| ())?;
            let actual = fstat(lock).map_err(|_| ())?;
            if bytes != expected
                || old.st_dev != actual.st_dev
                || old.st_ino != actual.st_ino
                || actual.st_nlink != 1
                || actual.st_mode & 0o077 != 0
                || actual.st_size != 0
            {
                return Err(());
            }
            Ok(())
        })();
        if checked.is_err() {
            *current = None;
        }
        checked
    }
    pub(crate) fn list(&self) -> Result<Vec<ImportAction>, ()> {
        let mut state = self.state.lock().map_err(|_| ())?;
        self.ready(&mut state)?;
        Ok(state.as_ref().ok_or(())?.actions.clone())
    }
    pub(crate) fn get(&self, id: Uuid) -> Result<ImportAction, ()> {
        self.list()?
            .into_iter()
            .find(|a| a.choice().is_ok_and(|c| c.action_id == id))
            .ok_or(())
    }
    /// Returns false for an existing exact choice; it must never be sent again.
    pub(crate) fn reserve(
        &self,
        proof: SignedImportApproval,
        source: ImportSource,
    ) -> Result<bool, ()> {
        let choice: ImportApproval = serde_json::from_str(&proof.payload).map_err(|_| ())?;
        verify_import_approval(&choice.bond, &proof).map_err(|_| ())?;
        let mut state = self.state.lock().map_err(|_| ())?;
        self.ready(&mut state)?;
        let current = state.as_ref().ok_or(())?;
        if let Some(old) = current
            .actions
            .iter()
            .find(|a| a.choice().is_ok_and(|c| c.action_id == choice.action_id))
        {
            return if old.proof == proof {
                Ok(false)
            } else {
                Err(())
            };
        }
        if choice.bond.controller != current.controller
            || source.source_id != choice.source_id
            || source.action_session != choice.action_session
            || source.action_sequence != choice.action_sequence
        {
            return Err(());
        }
        if choice.mode != covalent_core::appliance_import::ImportApprovalMode::Off
            && current
                .actions
                .iter()
                .any(|a| !a.resolved() && a.choice().is_ok_and(|c| c.bond == choice.bond))
        {
            return Err(());
        }
        let mut next = current.clone();
        if next.actions.len() >= MAX_RECORDS {
            let index = next
                .actions
                .iter()
                .enumerate()
                // The API has freshly verified this full bond. An older bond
                // cannot admit a delayed packet; its outcome stays unknown.
                .filter(|(_, a)| {
                    a.resolved()
                        || a.choice().is_ok_and(|c| {
                            c.bond.appliance == choice.bond.appliance && c.bond != choice.bond
                        })
                })
                .min_by_key(|(_, a)| a.order)
                .map(|(i, _)| i)
                .ok_or(())?;
            next.actions.remove(index);
        }
        next.order = next.order.checked_add(1).ok_or(())?;
        next.actions.push(ImportAction {
            order: next.order,
            proof,
            source,
            withdrawal_requested: false,
            withdrawal_signature: None,
            superseded_by: None,
            receipt: None,
        });
        self.persist(&mut state, next)?;
        Ok(true)
    }
    pub(crate) fn withdraw(&self, id: Uuid, signature: String) -> Result<bool, ()> {
        let mut state = self.state.lock().map_err(|_| ())?;
        self.ready(&mut state)?;
        let mut next = state.as_ref().ok_or(())?.clone();
        let action = next
            .actions
            .iter_mut()
            .find(|a| a.choice().is_ok_and(|c| c.action_id == id))
            .ok_or(())?;
        if action.resolved() || action.withdrawal_requested {
            return Ok(false);
        }
        let choice = action.choice()?;
        covalent_core::appliance_import::verify_import_withdrawal(
            &choice.bond,
            &action.proof,
            &signature,
        )
        .map_err(|_| ())?;
        action.withdrawal_requested = true;
        action.withdrawal_signature = Some(signature);
        self.persist(&mut state, next)?;
        Ok(true)
    }
    pub(crate) fn record(&self, id: Uuid, receipt: ImportReceipt) -> Result<ImportAction, ()> {
        let mut state = self.state.lock().map_err(|_| ())?;
        self.ready(&mut state)?;
        let mut next = state.as_ref().ok_or(())?.clone();
        let action = next
            .actions
            .iter_mut()
            .find(|a| a.choice().is_ok_and(|c| c.action_id == id))
            .ok_or(())?;
        receipt.validate_for(&action.proof).map_err(|_| ())?;
        if action.resolved() && action.receipt.as_ref() != Some(&receipt) {
            // Accepted may become expired on service restart, never unknown/uncertain/withdrawn.
            if !(action
                .receipt
                .as_ref()
                .is_some_and(|r| r.outcome == ImportOutcome::Accepted)
                && receipt.outcome == ImportOutcome::Expired)
            {
                return Ok(action.clone());
            }
        }
        action.receipt = Some(receipt);
        let result = action.clone();
        let choice = result.choice()?;
        if choice.mode == covalent_core::appliance_import::ImportApprovalMode::Off
            && result
                .receipt
                .as_ref()
                .is_some_and(|r| r.outcome == ImportOutcome::Accepted)
        {
            for earlier in &mut next.actions {
                if !earlier.resolved()
                    && earlier.order < result.order
                    && earlier.choice().is_ok_and(|c| {
                        c.bond.appliance == choice.bond.appliance && c.source_id == choice.source_id
                    })
                {
                    earlier.superseded_by = Some(id);
                }
            }
        }
        self.persist(&mut state, next)?;
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::appliance_imports::tests::{choice, fixture, receipt};
    use covalent_core::appliance_import::{
        IMPORT_WITHDRAWAL_DOMAIN, ImportApprovalMode, sign_import_approval,
    };
    use std::os::unix::fs::PermissionsExt as _;
    fn directory() -> tempfile::TempDir {
        tempfile::Builder::new()
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir()
            .unwrap()
    }
    #[test]
    fn persist_before_send_reopen_readback_and_withdraw_do_not_repeat_a_command() {
        let d = directory();
        let path = d.path().join("outbox.json");
        let (phone, context) = fixture();
        let c = choice(&context, ImportApprovalMode::Once);
        let proof = sign_import_approval(&phone, &c).unwrap();
        {
            let box_ = ImportOutbox::open(path.clone(), phone.public_identity()).unwrap();
            assert!(
                box_.reserve(proof.clone(), context.sources[0].clone())
                    .unwrap()
            );
            assert!(
                !box_
                    .reserve(proof.clone(), context.sources[0].clone())
                    .unwrap()
            );
            assert!(!box_.get(c.action_id).unwrap().resolved());
            assert!(ImportOutbox::open(path.clone(), phone.public_identity()).is_none());
        }
        let box_ = ImportOutbox::open(path.clone(), phone.public_identity()).unwrap();
        assert!(
            !box_
                .reserve(proof.clone(), context.sources[0].clone())
                .unwrap()
        );
        box_.record(c.action_id, receipt(&proof, ImportOutcome::Unknown))
            .unwrap();
        assert!(!box_.get(c.action_id).unwrap().resolved());
        assert!(box_.withdraw(c.action_id, proof.signature.clone()).is_err());
        let signature = phone.sign(IMPORT_WITHDRAWAL_DOMAIN, proof.payload.as_bytes());
        assert!(box_.withdraw(c.action_id, signature.clone()).unwrap());
        assert!(!box_.withdraw(c.action_id, signature).unwrap());
        box_.record(c.action_id, receipt(&proof, ImportOutcome::Withdrawn))
            .unwrap();
        assert!(box_.get(c.action_id).unwrap().resolved());
        box_.record(c.action_id, receipt(&proof, ImportOutcome::Unknown))
            .unwrap();
        assert_eq!(
            box_.get(c.action_id).unwrap().receipt.unwrap().outcome,
            ImportOutcome::Withdrawn
        );
    }
    #[test]
    fn damaged_missing_foreign_or_linked_state_never_resets_or_overwrites_pending_work() {
        for mode in [
            "missing",
            "corrupt",
            "permissions",
            "hardlink",
            "symlink",
            "foreign-controller",
        ] {
            let d = directory();
            let path = d.path().join("outbox.json");
            let (phone, c) = fixture();
            let p = sign_import_approval(&phone, &choice(&c, ImportApprovalMode::Once)).unwrap();
            let box_ = ImportOutbox::open(path.clone(), phone.public_identity()).unwrap();
            box_.reserve(p, c.sources[0].clone()).unwrap();
            match mode {
                "missing" => std::fs::remove_file(&path).unwrap(),
                "corrupt" => std::fs::write(&path, b"broken").unwrap(),
                "permissions" => {
                    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap()
                }
                "hardlink" => std::fs::hard_link(&path, d.path().join("alias")).unwrap(),
                "symlink" => {
                    std::fs::rename(&path, d.path().join("original")).unwrap();
                    std::os::unix::fs::symlink(d.path().join("original"), &path).unwrap();
                }
                _ => {
                    drop(box_);
                    assert!(ImportOutbox::open(path.clone(), c.bond.appliance).is_none());
                    continue;
                }
            }
            assert!(box_.list().is_err());
            drop(box_);
            assert!(
                ImportOutbox::open(path.clone(), phone.public_identity()).is_none(),
                "{mode}"
            );
        }
    }
    #[test]
    fn fresh_owner_bond_does_not_inherit_old_pending_admission_or_exhaust_its_history() {
        let d = directory();
        let (phone, mut context) = fixture();
        let box_ =
            ImportOutbox::open(d.path().join("outbox.json"), phone.public_identity()).unwrap();
        let original =
            sign_import_approval(&phone, &choice(&context, ImportApprovalMode::Once)).unwrap();
        box_.reserve(original.clone(), context.sources[0].clone())
            .unwrap();
        let duplicate =
            sign_import_approval(&phone, &choice(&context, ImportApprovalMode::Once)).unwrap();
        assert!(box_.reserve(duplicate, context.sources[0].clone()).is_err());
        // Each iteration stands for a separately verified current host bond.
        for _ in 0..40 {
            context.bond.root_generation = Uuid::new_v4();
            let proof =
                sign_import_approval(&phone, &choice(&context, ImportApprovalMode::Once)).unwrap();
            assert!(box_.reserve(proof, context.sources[0].clone()).unwrap());
            assert!(
                box_.list()
                    .unwrap()
                    .iter()
                    .all(|a| !a.resolved() && a.receipt.is_none())
            );
        }
        assert_eq!(box_.list().unwrap().len(), MAX_RECORDS);
        assert!(box_.list().unwrap().iter().all(|a| a.proof != original));
        let current =
            sign_import_approval(&phone, &choice(&context, ImportApprovalMode::Once)).unwrap();
        assert!(box_.reserve(current, context.sources[0].clone()).is_err());
    }
    #[test]
    fn pending_imports_allow_an_explicit_later_pause_as_safe_recovery() {
        let d = directory();
        let (phone, mut c) = fixture();
        let box_ = ImportOutbox::open(d.path().join("box.json"), phone.public_identity()).unwrap();
        let old = choice(&c, ImportApprovalMode::Remember);
        let proof = sign_import_approval(&phone, &old).unwrap();
        box_.reserve(proof, c.sources[0].clone()).unwrap();
        c.sources[0].action_sequence += 1;
        let blocked = sign_import_approval(&phone, &choice(&c, ImportApprovalMode::Once)).unwrap();
        assert!(box_.reserve(blocked, c.sources[0].clone()).is_err());
        let pause = choice(&c, ImportApprovalMode::Off);
        let proof = sign_import_approval(&phone, &pause).unwrap();
        box_.reserve(proof.clone(), c.sources[0].clone()).unwrap();
        box_.record(pause.action_id, receipt(&proof, ImportOutcome::Accepted))
            .unwrap();
        assert_eq!(
            box_.get(old.action_id).unwrap().superseded_by,
            Some(pause.action_id)
        );
        drop(box_);
        assert!(ImportOutbox::open(d.path().join("box.json"), phone.public_identity()).is_some());
    }
}
