//! UniFFI surface of `pigeonpost-groupcrypto`, shared by Swift, Kotlin, Python and C#.
//!
//! The binding mirrors the Rust API one to one. `GroupDevice` serializes access to the device
//! with a mutex, so a binding object can be shared across threads; callers still persist
//! `export_state` after each mutating call.

use std::sync::{Mutex, MutexGuard};

use pigeonpost_groupcrypto as core;

uniffi::setup_scaffolding!("pigeonpost_groupcrypto");

/// Every failure, with a stable machine code (see `docs/group-e2ee.md`).
#[derive(Debug, uniffi::Error)]
pub enum GroupCryptoError {
    /// Operation failed; `code` is stable, `message` is for logs.
    Failure {
        /// Stable code such as `stale_epoch` or `pending_commit`.
        code: String,
        /// Human-readable detail (never contains secrets).
        detail: String,
    },
}

impl std::error::Error for GroupCryptoError {}

impl std::fmt::Display for GroupCryptoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GroupCryptoError::Failure { code, detail } => write!(f, "{code}: {detail}"),
        }
    }
}

impl From<core::Error> for GroupCryptoError {
    fn from(e: core::Error) -> Self {
        GroupCryptoError::Failure {
            code: e.code().to_owned(),
            detail: e.to_string(),
        }
    }
}

type Result<T> = std::result::Result<T, GroupCryptoError>;

fn arr<const N: usize>(bytes: &[u8], what: &str) -> Result<[u8; N]> {
    bytes.try_into().map_err(|_| GroupCryptoError::Failure {
        code: "invalid_input".into(),
        detail: format!("{what} must be {N} bytes"),
    })
}

/// One device of a group.
#[derive(uniffi::Record, Clone)]
pub struct MemberInfo {
    pub mailbox: String,
    pub device_id: String,
    pub leaf_index: u32,
    pub signature_key: Vec<u8>,
    pub fingerprint: Vec<u8>,
}

impl From<core::MemberInfo> for MemberInfo {
    fn from(m: core::MemberInfo) -> Self {
        Self {
            mailbox: m.mailbox,
            device_id: m.device_id,
            leaf_index: m.leaf_index,
            signature_key: m.signature_key,
            fingerprint: m.fingerprint.to_vec(),
        }
    }
}

fn members(v: Vec<core::MemberInfo>) -> Vec<MemberInfo> {
    v.into_iter().map(Into::into).collect()
}

/// Identifies one device of one mailbox.
#[derive(uniffi::Record, Clone)]
pub struct DeviceRef {
    pub mailbox: String,
    pub device_id: String,
}

/// Reference to one encrypted attachment blob (contains its key: keep it secret).
#[derive(uniffi::Record, Clone)]
pub struct AttachmentRef {
    pub key: Vec<u8>,
    pub sealed_sha256: Vec<u8>,
    pub plaintext_len: u64,
}

impl From<&core::AttachmentRef> for AttachmentRef {
    fn from(a: &core::AttachmentRef) -> Self {
        Self {
            key: a.key.to_vec(),
            sealed_sha256: a.sealed_sha256.to_vec(),
            plaintext_len: a.plaintext_len,
        }
    }
}

impl TryFrom<&AttachmentRef> for core::AttachmentRef {
    type Error = GroupCryptoError;
    fn try_from(a: &AttachmentRef) -> Result<Self> {
        Ok(core::AttachmentRef {
            key: arr(&a.key, "attachment key")?,
            sealed_sha256: arr(&a.sealed_sha256, "attachment hash")?,
            plaintext_len: a.plaintext_len,
        })
    }
}

/// A sealed attachment: upload `sealed`, embed `reference` in the message.
#[derive(uniffi::Record)]
pub struct SealedAttachment {
    pub sealed: Vec<u8>,
    pub reference: AttachmentRef,
}

/// A verified key package.
#[derive(uniffi::Record)]
pub struct KeyPackageInfo {
    pub key_package_ref: Vec<u8>,
    pub mailbox: String,
    pub device_id: String,
    pub signature_key: Vec<u8>,
    pub fingerprint: Vec<u8>,
    pub last_resort: bool,
    pub not_after: u64,
}

/// Cleartext header of a protected group message.
#[derive(uniffi::Record)]
pub struct EnvelopeInfo {
    pub group_id: String,
    pub epoch: u64,
    /// `application` or `commit`.
    pub kind: String,
    pub sender_mailbox: String,
    pub sender_device: String,
    pub client_message_id: String,
}

/// A commit to post; then `confirm_commit` or `discard_commit`.
#[derive(uniffi::Record)]
pub struct CommitBundle {
    pub group_id: String,
    pub epoch: u64,
    pub commit_id: String,
    pub commit: Vec<u8>,
    pub welcome: Option<Vec<u8>>,
    pub welcome_recipients: Vec<Vec<u8>>,
    pub envelope_signature: Vec<u8>,
    pub added: Vec<MemberInfo>,
    pub removed: Vec<MemberInfo>,
}

impl From<core::CommitBundle> for CommitBundle {
    fn from(b: core::CommitBundle) -> Self {
        Self {
            group_id: b.group_id,
            epoch: b.epoch,
            commit_id: b.commit_id,
            commit: b.commit,
            welcome: b.welcome,
            welcome_recipients: b.welcome_recipients,
            envelope_signature: b.envelope_signature,
            added: members(b.added),
            removed: members(b.removed),
        }
    }
}

/// An encrypted application message to post.
#[derive(uniffi::Record)]
pub struct OutgoingMessage {
    pub group_id: String,
    pub epoch: u64,
    pub client_message_id: String,
    pub ciphertext: Vec<u8>,
    pub envelope_signature: Vec<u8>,
}

impl From<core::OutgoingMessage> for OutgoingMessage {
    fn from(m: core::OutgoingMessage) -> Self {
        Self {
            group_id: m.group_id,
            epoch: m.epoch,
            client_message_id: m.client_message_id,
            ciphertext: m.ciphertext,
            envelope_signature: m.envelope_signature,
        }
    }
}

/// A membership change applied locally.
#[derive(uniffi::Record)]
pub struct CommitOutcome {
    pub group_id: String,
    pub committer: MemberInfo,
    pub commit_id: String,
    pub epoch: u64,
    pub added: Vec<MemberInfo>,
    pub removed: Vec<MemberInfo>,
    pub updated: Vec<MemberInfo>,
    pub self_removed: bool,
}

impl From<core::CommitOutcome> for CommitOutcome {
    fn from(o: core::CommitOutcome) -> Self {
        Self {
            group_id: o.group_id,
            committer: o.committer.into(),
            commit_id: o.commit_id,
            epoch: o.epoch,
            added: members(o.added),
            removed: members(o.removed),
            updated: members(o.updated),
            self_removed: o.self_removed,
        }
    }
}

/// A decrypted application message.
#[derive(uniffi::Record)]
pub struct ReceivedMessage {
    pub group_id: String,
    pub sender: MemberInfo,
    pub epoch: u64,
    pub client_message_id: String,
    pub body: Vec<u8>,
    pub attachments: Vec<AttachmentRef>,
}

/// An offer of history from another of this mailbox's own devices.
#[derive(uniffi::Record)]
pub struct HistoryOffer {
    pub group_id: String,
    pub sender: MemberInfo,
    pub client_message_id: String,
    pub archive: AttachmentRef,
}

/// Result of processing one incoming message.
#[derive(uniffi::Enum)]
pub enum Incoming {
    Message { message: ReceivedMessage },
    HistoryOffer { offer: HistoryOffer },
    Commit { outcome: CommitOutcome },
    OwnCommitConfirmed { outcome: CommitOutcome },
    Own,
}

/// A group just created or joined.
#[derive(uniffi::Record)]
pub struct Joined {
    pub group_id: String,
    pub epoch: u64,
    pub members: Vec<MemberInfo>,
}

impl From<core::Joined> for Joined {
    fn from(j: core::Joined) -> Self {
        Self {
            group_id: j.group_id,
            epoch: j.epoch,
            members: members(j.members),
        }
    }
}

/// A sealed history archive and the message carrying its key.
#[derive(uniffi::Record)]
pub struct HistoryTransfer {
    pub sealed_archive: Vec<u8>,
    pub message: OutgoingMessage,
}

/// One enrolled device and all of its group state.
#[derive(uniffi::Object)]
pub struct GroupDevice {
    inner: Mutex<core::Device>,
}

impl GroupDevice {
    fn lock(&self) -> MutexGuard<'_, core::Device> {
        // A panic inside the core never leaves the device half-updated in a way the mutex
        // could protect against, so recover the guard rather than poisoning the binding.
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }
}

fn wrap(d: core::Device) -> std::sync::Arc<GroupDevice> {
    std::sync::Arc::new(GroupDevice {
        inner: Mutex::new(d),
    })
}

#[uniffi::export]
impl GroupDevice {
    /// New unattested device for `mailbox` (a `/k/` address).
    #[uniffi::constructor]
    pub fn new(mailbox: String, device_id: String) -> Result<std::sync::Arc<Self>> {
        Ok(wrap(core::Device::new(&mailbox, &device_id)?))
    }

    /// New device attested with the mailbox identity seed (CLI/agentd/tests).
    #[uniffi::constructor]
    pub fn new_with_identity_seed(
        identity_seed: Vec<u8>,
        device_id: String,
    ) -> Result<std::sync::Arc<Self>> {
        Ok(wrap(core::Device::new_with_identity_seed(
            &arr(&identity_seed, "identity seed")?,
            &device_id,
        )?))
    }

    /// Restore from `export_state`.
    #[uniffi::constructor]
    pub fn import_state(blob: Vec<u8>, key: Vec<u8>) -> Result<std::sync::Arc<Self>> {
        Ok(wrap(core::Device::import_state(&blob, &key)?))
    }

    pub fn export_state(&self, key: Vec<u8>) -> Result<Vec<u8>> {
        Ok(self.lock().export_state(&key)?)
    }

    pub fn attestation_payload(&self) -> Result<Vec<u8>> {
        Ok(self.lock().attestation_payload()?)
    }

    pub fn install_attestation(
        &self,
        mailbox_public_key: Vec<u8>,
        signature: Vec<u8>,
    ) -> Result<()> {
        Ok(self
            .lock()
            .install_attestation(&mailbox_public_key, &signature)?)
    }

    pub fn mailbox(&self) -> String {
        self.lock().mailbox().to_owned()
    }

    pub fn device_id(&self) -> String {
        self.lock().device_id().to_owned()
    }

    pub fn signature_public_key(&self) -> Vec<u8> {
        self.lock().signature_public_key()
    }

    pub fn fingerprint(&self) -> Vec<u8> {
        self.lock().fingerprint().to_vec()
    }

    pub fn is_attested(&self) -> bool {
        self.lock().is_attested()
    }

    pub fn groups(&self) -> Vec<String> {
        self.lock().groups()
    }

    pub fn generate_key_packages(&self, count: u32) -> Result<Vec<Vec<u8>>> {
        Ok(self.lock().generate_key_packages(count)?)
    }

    pub fn generate_last_resort_key_package(&self) -> Result<Vec<u8>> {
        Ok(self.lock().generate_last_resort_key_package()?)
    }

    pub fn create_group(&self, group_id: String) -> Result<Joined> {
        Ok(self.lock().create_group(&group_id)?.into())
    }

    pub fn join_from_welcome(&self, welcome: Vec<u8>) -> Result<Joined> {
        Ok(self.lock().join_from_welcome(&welcome)?.into())
    }

    pub fn members(&self, group_id: String) -> Result<Vec<MemberInfo>> {
        Ok(members(self.lock().members(&group_id)?))
    }

    pub fn epoch(&self, group_id: String) -> Result<u64> {
        Ok(self.lock().epoch(&group_id)?)
    }

    pub fn has_pending_commit(&self, group_id: String) -> Result<bool> {
        Ok(self.lock().has_pending_commit(&group_id)?)
    }

    pub fn safety_number(&self, group_id: String) -> Result<String> {
        Ok(self.lock().safety_number(&group_id)?)
    }

    pub fn epoch_authenticator(&self, group_id: String) -> Result<Vec<u8>> {
        Ok(self.lock().epoch_authenticator(&group_id)?)
    }

    pub fn commit(
        &self,
        group_id: String,
        key_packages: Vec<Vec<u8>>,
        remove: Vec<DeviceRef>,
    ) -> Result<CommitBundle> {
        let remove: Vec<core::DeviceRef> = remove
            .into_iter()
            .map(|r| core::DeviceRef {
                mailbox: r.mailbox,
                device_id: r.device_id,
            })
            .collect();
        Ok(self
            .lock()
            .commit(&group_id, &key_packages, &remove)?
            .into())
    }

    pub fn add_members(
        &self,
        group_id: String,
        key_packages: Vec<Vec<u8>>,
    ) -> Result<CommitBundle> {
        Ok(self.lock().add_members(&group_id, &key_packages)?.into())
    }

    pub fn remove_mailbox(&self, group_id: String, mailbox: String) -> Result<CommitBundle> {
        Ok(self.lock().remove_mailbox(&group_id, &mailbox)?.into())
    }

    pub fn self_update(&self, group_id: String) -> Result<CommitBundle> {
        Ok(self.lock().self_update(&group_id)?.into())
    }

    pub fn confirm_commit(&self, group_id: String) -> Result<CommitOutcome> {
        Ok(self.lock().confirm_commit(&group_id)?.into())
    }

    pub fn discard_commit(&self, group_id: String) -> Result<()> {
        Ok(self.lock().discard_commit(&group_id)?)
    }

    pub fn encrypt_message(
        &self,
        group_id: String,
        client_message_id: String,
        body: Vec<u8>,
        attachments: Vec<AttachmentRef>,
    ) -> Result<OutgoingMessage> {
        let attachments = attachments
            .iter()
            .map(TryInto::try_into)
            .collect::<Result<Vec<core::AttachmentRef>>>()?;
        Ok(self
            .lock()
            .encrypt_message(&group_id, &client_message_id, &body, &attachments)?
            .into())
    }

    pub fn process_incoming(&self, group_id: String, message: Vec<u8>) -> Result<Incoming> {
        Ok(match self.lock().process_incoming(&group_id, &message)? {
            core::Incoming::Message(m) => Incoming::Message {
                message: ReceivedMessage {
                    attachments: m.attachments.iter().map(Into::into).collect(),
                    group_id: m.group_id,
                    sender: m.sender.into(),
                    epoch: m.epoch,
                    client_message_id: m.client_message_id,
                    body: m.body,
                },
            },
            core::Incoming::HistoryOffer(o) => Incoming::HistoryOffer {
                offer: HistoryOffer {
                    archive: (&o.archive).into(),
                    group_id: o.group_id,
                    sender: o.sender.into(),
                    client_message_id: o.client_message_id,
                },
            },
            core::Incoming::Commit(o) => Incoming::Commit { outcome: o.into() },
            core::Incoming::OwnCommitConfirmed(o) => {
                Incoming::OwnCommitConfirmed { outcome: o.into() }
            }
            core::Incoming::Own => Incoming::Own,
        })
    }

    pub fn seal_history(
        &self,
        self_group_id: String,
        client_message_id: String,
        archive: Vec<u8>,
    ) -> Result<HistoryTransfer> {
        let t = self
            .lock()
            .seal_history(&self_group_id, &client_message_id, &archive)?;
        Ok(HistoryTransfer {
            sealed_archive: t.sealed_archive,
            message: t.message.into(),
        })
    }

    pub fn open_history(&self, offer: HistoryOffer, sealed_archive: Vec<u8>) -> Result<Vec<u8>> {
        let device = self.lock();
        let core_offer = core::HistoryOffer {
            group_id: offer.group_id,
            sender: core::MemberInfo {
                mailbox: offer.sender.mailbox,
                device_id: offer.sender.device_id,
                leaf_index: offer.sender.leaf_index,
                signature_key: offer.sender.signature_key,
                fingerprint: arr(&offer.sender.fingerprint, "fingerprint")?,
            },
            client_message_id: offer.client_message_id,
            archive: (&offer.archive).try_into()?,
        };
        Ok(device.open_history(&core_offer, &sealed_archive)?)
    }

    pub fn forget_group(&self, group_id: String) -> Result<()> {
        Ok(self.lock().forget_group(&group_id)?)
    }
}

/// Validate and describe an uploaded key package.
#[uniffi::export]
pub fn inspect_key_package(key_package: Vec<u8>) -> Result<KeyPackageInfo> {
    let i = core::inspect_key_package(&key_package)?;
    Ok(KeyPackageInfo {
        key_package_ref: i.key_package_ref,
        mailbox: i.mailbox,
        device_id: i.device_id,
        signature_key: i.signature_key,
        fingerprint: i.fingerprint.to_vec(),
        last_resort: i.last_resort,
        not_after: i.not_after,
    })
}

/// Parse the cleartext header of a group message.
#[uniffi::export]
pub fn inspect_envelope(message: Vec<u8>) -> Result<EnvelopeInfo> {
    let i = core::inspect_envelope(&message)?;
    Ok(EnvelopeInfo {
        group_id: i.group_id,
        epoch: i.epoch,
        kind: match i.kind {
            core::MessageKind::Application => "application".into(),
            core::MessageKind::Commit => "commit".into(),
        },
        sender_mailbox: i.sender_mailbox,
        sender_device: i.sender_device,
        client_message_id: i.client_message_id,
    })
}

/// Verify a detached envelope signature against a device signature key.
#[uniffi::export]
pub fn verify_envelope_signature(
    device_signature_key: Vec<u8>,
    message: Vec<u8>,
    signature: Vec<u8>,
) -> Result<()> {
    Ok(core::verify_envelope_signature(
        &device_signature_key,
        &message,
        &signature,
    )?)
}

/// KeyPackageRefs a Welcome is addressed to.
#[uniffi::export]
pub fn welcome_recipients(welcome: Vec<u8>) -> Result<Vec<Vec<u8>>> {
    Ok(core::welcome_recipients(&welcome)?)
}

/// Seal an attachment under a fresh key.
#[uniffi::export]
pub fn seal_attachment(plaintext: Vec<u8>) -> Result<SealedAttachment> {
    let s = core::seal_attachment(&plaintext)?;
    Ok(SealedAttachment {
        reference: (&s.reference).into(),
        sealed: s.sealed,
    })
}

/// Open an attachment after checking its hash.
#[uniffi::export]
pub fn open_attachment(sealed: Vec<u8>, reference: AttachmentRef) -> Result<Vec<u8>> {
    Ok(core::open_attachment(&sealed, &(&reference).try_into()?)?)
}

/// 32 random bytes for `export_state`.
#[uniffi::export]
pub fn generate_state_key() -> Result<Vec<u8>> {
    Ok(core::generate_state_key()?)
}

/// Six groups of five digits for a device fingerprint.
#[uniffi::export]
pub fn fingerprint_digits(fingerprint: Vec<u8>) -> Result<String> {
    Ok(core::fingerprint_digits(&arr(&fingerprint, "fingerprint")?))
}

/// `/k/` mailbox address of an identity seed.
#[uniffi::export]
pub fn mailbox_for_seed(identity_seed: Vec<u8>) -> Result<String> {
    Ok(core::mailbox_for_seed(&arr(
        &identity_seed,
        "identity seed",
    )?))
}

/// Sign a device attestation payload with a mailbox identity seed.
#[uniffi::export]
pub fn sign_attestation_with_seed(identity_seed: Vec<u8>, payload: Vec<u8>) -> Result<Vec<u8>> {
    Ok(
        core::sign_attestation_with_seed(&arr(&identity_seed, "identity seed")?, &payload)?
            .to_vec(),
    )
}

/// Protocol framing version string (`enc_scheme`).
#[uniffi::export]
pub fn enc_scheme() -> String {
    core::ENC_SCHEME.to_owned()
}
