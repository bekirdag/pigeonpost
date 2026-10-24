//! The device: one enrolled client of one mailbox, holding all of its MLS group state.

use std::collections::{BTreeMap, BTreeSet};

use openmls::prelude::tls_codec::{Deserialize as _, Serialize as _};
use openmls::prelude::*;
use openmls_basic_credential::SignatureKeyPair;
use openmls_memory_storage::MemoryStorage;
use openmls_rust_crypto::RustCrypto;
use openmls_traits::signatures::Signer as _;
use openmls_traits::OpenMlsProvider;
use sha2::{Digest, Sha256};

use crate::attachment::{open_attachment, seal_attachment, AttachmentRef};
use crate::credential::{
    attestation_payload, canonical_mailbox, device_fingerprint, mailbox_for_seed, safety_number,
    sign_attestation_with_seed, validate_device_id, DeviceCredential,
};
use crate::error::{Error, Result};
use crate::payload::{
    envelope_signing_input, validate_client_message_id, validate_group_id, MessageAad, MessageKind,
    Payload, PayloadKind,
};
use crate::random::random_array;

/// RFC 9420 mandatory-to-implement suite; the only one this crate accepts.
pub const CIPHERSUITE: Ciphersuite = Ciphersuite::MLS_128_DHKEMX25519_AES128GCM_SHA256_Ed25519;
/// Most key packages generated per call.
pub const MAX_KEY_PACKAGES_PER_CALL: u32 = 100;
const PADDING: usize = 32;

/// OpenMLS provider: RustCrypto primitives plus an in-memory store this crate serializes.
#[derive(Default)]
pub(crate) struct Provider {
    crypto: RustCrypto,
    pub(crate) storage: MemoryStorage,
}

impl OpenMlsProvider for Provider {
    type CryptoProvider = RustCrypto;
    type RandProvider = RustCrypto;
    type StorageProvider = MemoryStorage;

    fn storage(&self) -> &MemoryStorage {
        &self.storage
    }
    fn crypto(&self) -> &RustCrypto {
        &self.crypto
    }
    fn rand(&self) -> &RustCrypto {
        &self.crypto
    }
}

/// One member device of a group, as shown to users.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MemberInfo {
    /// Mailbox address (`/k/…`).
    pub mailbox: String,
    /// Device id within that mailbox.
    pub device_id: String,
    /// MLS leaf index (stable while the device stays in the group).
    pub leaf_index: u32,
    /// The device's MLS signature public key.
    pub signature_key: Vec<u8>,
    /// [`device_fingerprint`] of this device.
    pub fingerprint: [u8; 32],
}

/// Identifies one device of one mailbox.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct DeviceRef {
    /// Mailbox address.
    pub mailbox: String,
    /// Device id.
    pub device_id: String,
}

/// A verified key package, as the server or an inviter sees it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyPackageInfo {
    /// RFC 9420 KeyPackageRef (hash); Welcome messages name recipients by it.
    pub key_package_ref: Vec<u8>,
    /// Mailbox the device belongs to.
    pub mailbox: String,
    /// Device id.
    pub device_id: String,
    /// Device MLS signature key (also the key that signs this device's envelopes).
    pub signature_key: Vec<u8>,
    /// Device fingerprint.
    pub fingerprint: [u8; 32],
    /// Last-resort packages may be handed out more than once.
    pub last_resort: bool,
    /// Lifetime end (Unix seconds).
    pub not_after: u64,
}

/// A commit this device created. Send it to the server; then call
/// [`Device::confirm_commit`] if accepted or [`Device::discard_commit`] if rejected.
#[derive(Clone, Debug)]
pub struct CommitBundle {
    /// Group id.
    pub group_id: String,
    /// Epoch the commit was built on (the server's expected epoch).
    pub epoch: u64,
    /// Idempotency key also bound into the commit's authenticated data.
    pub commit_id: String,
    /// Serialized MLS commit (a `PrivateMessage`).
    pub commit: Vec<u8>,
    /// Serialized Welcome for added devices, if any.
    pub welcome: Option<Vec<u8>>,
    /// KeyPackageRefs the Welcome is addressed to.
    pub welcome_recipients: Vec<Vec<u8>>,
    /// Detached device signature over `commit` for the server.
    pub envelope_signature: Vec<u8>,
    /// Devices this commit adds.
    pub added: Vec<MemberInfo>,
    /// Devices this commit removes.
    pub removed: Vec<MemberInfo>,
}

/// An encrypted application message ready to post.
#[derive(Clone, Debug)]
pub struct OutgoingMessage {
    /// Group id.
    pub group_id: String,
    /// Epoch the message was encrypted in.
    pub epoch: u64,
    /// The client message id bound into the message.
    pub client_message_id: String,
    /// Serialized MLS `PrivateMessage`.
    pub ciphertext: Vec<u8>,
    /// Detached device signature over `ciphertext` for the server.
    pub envelope_signature: Vec<u8>,
}

/// A membership change applied to local state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommitOutcome {
    /// Group id.
    pub group_id: String,
    /// Device that made the commit.
    pub committer: MemberInfo,
    /// Commit id from its authenticated data.
    pub commit_id: String,
    /// New epoch after the commit.
    pub epoch: u64,
    /// Devices added.
    pub added: Vec<MemberInfo>,
    /// Devices removed.
    pub removed: Vec<MemberInfo>,
    /// Devices whose keys were updated (including the committer's path update).
    pub updated: Vec<MemberInfo>,
    /// This device was removed; its local state for the group has been erased.
    pub self_removed: bool,
}

/// A decrypted application message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReceivedMessage {
    /// Group id.
    pub group_id: String,
    /// Authenticated sender device.
    pub sender: MemberInfo,
    /// Epoch it was sent in.
    pub epoch: u64,
    /// Sender's client message id (dedupe on `(sender mailbox, client_message_id)`).
    pub client_message_id: String,
    /// Application body (Pigeonpost's JSON message document).
    pub body: Vec<u8>,
    /// Attachment references (keys inside).
    pub attachments: Vec<AttachmentRef>,
}

/// An offer of message history from another of this mailbox's own devices.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryOffer {
    /// Self group the offer arrived in.
    pub group_id: String,
    /// Which own device sent it.
    pub sender: MemberInfo,
    /// Client message id.
    pub client_message_id: String,
    /// Reference to the sealed history archive blob.
    pub archive: AttachmentRef,
}

/// Result of processing one incoming group message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Incoming {
    /// An application message.
    Message(ReceivedMessage),
    /// A history transfer offer (only in a self group).
    HistoryOffer(HistoryOffer),
    /// A commit from another device, already validated and merged.
    Commit(CommitOutcome),
    /// This device's own commit echoed back; it matched the pending commit and is now merged.
    OwnCommitConfirmed(CommitOutcome),
    /// One of this device's own messages echoed back; nothing to do.
    Own,
}

/// A group this device just joined.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Joined {
    /// Group id.
    pub group_id: String,
    /// Epoch at join.
    pub epoch: u64,
    /// Current members.
    pub members: Vec<MemberInfo>,
}

/// History archive sealed for this mailbox's own other devices.
#[derive(Clone, Debug)]
pub struct HistoryTransfer {
    /// Sealed archive blob to upload.
    pub sealed_archive: Vec<u8>,
    /// MLS message carrying the archive key, to post to the self group.
    pub message: OutgoingMessage,
}

/// One enrolled device of one mailbox. Not `Clone`: there must be exactly one live copy of
/// the ratchet state. Persist with [`Device::export_state`] after every mutating call.
pub struct Device {
    pub(crate) provider: Provider,
    pub(crate) signer: SignatureKeyPair,
    pub(crate) mailbox: String,
    pub(crate) device_id: String,
    pub(crate) credential: Option<DeviceCredential>,
    pub(crate) groups: BTreeSet<String>,
    /// group id -> (SHA-256 of the pending commit message, commit id), to recognise its echo.
    pub(crate) pending_commits: BTreeMap<String, ([u8; 32], String)>,
}

fn validate_group_name(group_id: &str) -> Result<()> {
    validate_group_id(group_id.as_bytes())?;
    if !group_id.bytes().all(|b| b.is_ascii_graphic()) {
        return Err(Error::invalid("group id must be printable ASCII"));
    }
    Ok(())
}

fn group_id_string(id: &GroupId) -> Result<String> {
    let s =
        core::str::from_utf8(id.as_slice()).map_err(|_| Error::Decode("group id is not UTF-8"))?;
    validate_group_name(s)?;
    Ok(s.to_owned())
}

/// Leaf capabilities: defaults plus the last-resort key package extension.
fn capabilities() -> Capabilities {
    Capabilities::builder()
        .extensions(vec![ExtensionType::LastResort])
        .build()
}

fn create_config() -> MlsGroupCreateConfig {
    MlsGroupCreateConfig::builder()
        .ciphersuite(CIPHERSUITE)
        .capabilities(capabilities())
        .use_ratchet_tree_extension(true)
        .wire_format_policy(PURE_CIPHERTEXT_WIRE_FORMAT_POLICY)
        .padding_size(PADDING)
        .max_past_epochs(0)
        .build()
}

fn join_config() -> MlsGroupJoinConfig {
    MlsGroupJoinConfig::builder()
        .use_ratchet_tree_extension(true)
        .wire_format_policy(PURE_CIPHERTEXT_WIRE_FORMAT_POLICY)
        .padding_size(PADDING)
        .max_past_epochs(0)
        .build()
}

fn basic_identity(credential: &Credential) -> Result<Vec<u8>> {
    BasicCredential::try_from(credential.clone())
        .map(|c| c.identity().to_vec())
        .map_err(|_| Error::InvalidCredential("not a basic credential".into()))
}

/// Verify a leaf's credential and build its [`MemberInfo`].
fn member_info(
    credential: &Credential,
    signature_key: &[u8],
    leaf_index: u32,
) -> Result<MemberInfo> {
    let identity = basic_identity(credential)?;
    let dc = DeviceCredential::from_leaf(&identity, signature_key)?;
    Ok(MemberInfo {
        fingerprint: device_fingerprint(&dc.mailbox, &dc.device_id, signature_key),
        mailbox: dc.mailbox,
        device_id: dc.device_id,
        leaf_index,
        signature_key: signature_key.to_vec(),
    })
}

fn leaf_member_info(leaf: &LeafNode, leaf_index: u32) -> Result<MemberInfo> {
    member_info(
        leaf.credential(),
        leaf.signature_key().as_slice(),
        leaf_index,
    )
}

fn parse_message(bytes: &[u8]) -> Result<MlsMessageBodyIn> {
    let mut input = bytes;
    let message = MlsMessageIn::tls_deserialize(&mut input)
        .map_err(|_| Error::Decode("not an MLS message"))?;
    if !input.is_empty() {
        return Err(Error::Decode("trailing bytes after MLS message"));
    }
    Ok(message.extract())
}

fn serialize(message: &MlsMessageOut) -> Result<Vec<u8>> {
    message.tls_serialize_detached().map_err(Error::mls)
}

fn new_id(prefix: &str) -> Result<String> {
    let bytes = random_array::<16>()?;
    Ok(format!(
        "{prefix}{}",
        bytes.iter().map(|b| format!("{b:02x}")).collect::<String>()
    ))
}

/// Validate and describe a serialized key package (`MlsMessage` wrapping a `KeyPackage`).
/// Pure function: the postbox can run it when a device uploads key packages.
pub fn inspect_key_package(bytes: &[u8]) -> Result<KeyPackageInfo> {
    let crypto = RustCrypto::default();
    let key_package = match parse_message(bytes)? {
        MlsMessageBodyIn::KeyPackage(kp) => kp
            .validate(&crypto, ProtocolVersion::Mls10)
            .map_err(|e| Error::InvalidCredential(format!("key package: {e:?}")))?,
        _ => return Err(Error::Unsupported("expected a key package")),
    };
    describe_key_package(&key_package, &crypto)
}

fn describe_key_package(key_package: &KeyPackage, crypto: &RustCrypto) -> Result<KeyPackageInfo> {
    if key_package.ciphersuite() != CIPHERSUITE {
        return Err(Error::Unsupported("key package ciphersuite"));
    }
    let leaf = key_package.leaf_node();
    let info = leaf_member_info(leaf, 0)?;
    Ok(KeyPackageInfo {
        key_package_ref: key_package
            .hash_ref(crypto)
            .map_err(Error::mls)?
            .as_slice()
            .to_vec(),
        mailbox: info.mailbox,
        device_id: info.device_id,
        signature_key: info.signature_key,
        fingerprint: info.fingerprint,
        last_resort: key_package.last_resort(),
        not_after: key_package.life_time().not_after(),
    })
}

/// Server-side view of a protected group message: everything visible without keys.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EnvelopeInfo {
    /// Group id.
    pub group_id: String,
    /// Epoch.
    pub epoch: u64,
    /// Application or commit, from the MLS framing (cross-checked against the AAD).
    pub kind: MessageKind,
    /// Declared sender mailbox (authenticated for recipients; server checks it against the
    /// posting identity and the envelope signature).
    pub sender_mailbox: String,
    /// Declared sender device.
    pub sender_device: String,
    /// Client message id / commit id.
    pub client_message_id: String,
}

/// Parse the cleartext header of a group `PrivateMessage` (for the postbox).
pub fn inspect_envelope(bytes: &[u8]) -> Result<EnvelopeInfo> {
    let protocol = match parse_message(bytes)? {
        MlsMessageBodyIn::PrivateMessage(m) => m,
        _ => {
            return Err(Error::Unsupported(
                "group messages must be MLS PrivateMessages",
            ))
        }
    };
    let aad = MessageAad::decode(protocol.aad())?;
    let framed = match protocol.content_type() {
        ContentType::Application => MessageKind::Application,
        ContentType::Commit => MessageKind::Commit,
        ContentType::Proposal => return Err(Error::Unsupported("standalone proposals")),
    };
    if framed != aad.kind {
        return Err(Error::BindingMismatch(
            "content type differs from authenticated data",
        ));
    }
    let group_id = group_id_string(protocol.group_id())?;
    if aad.group_id != group_id.as_bytes() || aad.epoch != protocol.epoch().as_u64() {
        return Err(Error::BindingMismatch(
            "authenticated data differs from MLS header",
        ));
    }
    Ok(EnvelopeInfo {
        group_id,
        epoch: aad.epoch,
        kind: aad.kind,
        sender_mailbox: aad.sender_mailbox,
        sender_device: aad.sender_device,
        client_message_id: aad.client_message_id,
    })
}

/// KeyPackageRefs a Welcome is addressed to (for routing by the postbox).
pub fn welcome_recipients(bytes: &[u8]) -> Result<Vec<Vec<u8>>> {
    match parse_message(bytes)? {
        MlsMessageBodyIn::Welcome(w) => Ok(w
            .secrets()
            .iter()
            .map(|s| s.new_member().as_slice().to_vec())
            .collect()),
        _ => Err(Error::Unsupported("expected a welcome")),
    }
}

impl Device {
    /// Create a new device for `mailbox` with a fresh MLS signature key. The device must be
    /// attested ([`Device::install_attestation`]) before it can publish key packages or join.
    pub fn new(mailbox: &str, device_id: &str) -> Result<Self> {
        let mailbox = canonical_mailbox(mailbox)?;
        validate_device_id(device_id)?;
        let signer =
            SignatureKeyPair::new(CIPHERSUITE.signature_algorithm()).map_err(Error::mls)?;
        Ok(Self {
            provider: Provider::default(),
            signer,
            mailbox,
            device_id: device_id.to_owned(),
            credential: None,
            groups: BTreeSet::new(),
            pending_commits: BTreeMap::new(),
        })
    }

    /// Convenience for clients that hold the mailbox identity seed (CLI, agentd, tests):
    /// create and attest in one step.
    pub fn new_with_identity_seed(identity_seed: &[u8; 32], device_id: &str) -> Result<Self> {
        let mut device = Self::new(&mailbox_for_seed(identity_seed), device_id)?;
        let payload = device.attestation_payload()?;
        let signature = sign_attestation_with_seed(identity_seed, &payload)?;
        let public = ed25519_dalek::SigningKey::from_bytes(identity_seed)
            .verifying_key()
            .to_bytes();
        device.install_attestation(&public, &signature)?;
        Ok(device)
    }

    /// Bytes the mailbox identity key must sign to attest this device.
    pub fn attestation_payload(&self) -> Result<Vec<u8>> {
        attestation_payload(&self.mailbox, &self.device_id, self.signer.public())
    }

    /// Install (and verify) the mailbox's signature over [`Device::attestation_payload`].
    pub fn install_attestation(
        &mut self,
        mailbox_public_key: &[u8],
        signature: &[u8],
    ) -> Result<()> {
        let credential = DeviceCredential {
            mailbox: self.mailbox.clone(),
            device_id: self.device_id.clone(),
            mailbox_public_key: mailbox_public_key
                .try_into()
                .map_err(|_| Error::invalid("mailbox public key must be 32 bytes"))?,
            attestation: signature
                .try_into()
                .map_err(|_| Error::invalid("signature must be 64 bytes"))?,
        };
        credential.verify(self.signer.public())?;
        self.credential = Some(credential);
        Ok(())
    }

    /// Mailbox address.
    pub fn mailbox(&self) -> &str {
        &self.mailbox
    }

    /// Device id.
    pub fn device_id(&self) -> &str {
        &self.device_id
    }

    /// The device's MLS signature public key (envelope signatures verify against it).
    pub fn signature_public_key(&self) -> Vec<u8> {
        self.signer.public().to_vec()
    }

    /// This device's fingerprint (compare with [`crate::fingerprint_digits`]).
    pub fn fingerprint(&self) -> [u8; 32] {
        device_fingerprint(&self.mailbox, &self.device_id, self.signer.public())
    }

    /// Whether the device is attested.
    pub fn is_attested(&self) -> bool {
        self.credential.is_some()
    }

    /// Group ids this device holds state for.
    pub fn groups(&self) -> Vec<String> {
        self.groups.iter().cloned().collect()
    }

    fn credential_with_key(&self) -> Result<CredentialWithKey> {
        let credential = self.credential.as_ref().ok_or(Error::NotAttested)?;
        Ok(CredentialWithKey {
            credential: BasicCredential::new(credential.encode()?).into(),
            signature_key: self.signer.public().into(),
        })
    }

    fn sign_envelope(&self, message: &[u8]) -> Result<Vec<u8>> {
        self.signer
            .sign(&envelope_signing_input(message))
            .map_err(Error::mls)
    }

    fn build_key_package(&self, last_resort: bool) -> Result<Vec<u8>> {
        let mut builder = KeyPackage::builder().leaf_node_capabilities(capabilities());
        if last_resort {
            builder = builder.mark_as_last_resort();
        }
        let bundle = builder
            .build(
                CIPHERSUITE,
                &self.provider,
                &self.signer,
                self.credential_with_key()?,
            )
            .map_err(Error::mls)?;
        serialize(&MlsMessageOut::from(bundle.key_package().clone()))
    }

    /// Generate `count` single-use key packages to publish. Their private keys stay in state.
    pub fn generate_key_packages(&mut self, count: u32) -> Result<Vec<Vec<u8>>> {
        if count == 0 || count > MAX_KEY_PACKAGES_PER_CALL {
            return Err(Error::invalid("key package count must be 1-100"));
        }
        (0..count).map(|_| self.build_key_package(false)).collect()
    }

    /// Generate a last-resort key package, handed out only when single-use ones run out.
    pub fn generate_last_resort_key_package(&mut self) -> Result<Vec<u8>> {
        self.build_key_package(true)
    }

    fn load(&self, group_id: &str) -> Result<MlsGroup> {
        validate_group_name(group_id)?;
        if !self.groups.contains(group_id) {
            return Err(Error::UnknownGroup);
        }
        MlsGroup::load(
            self.provider.storage(),
            &GroupId::from_slice(group_id.as_bytes()),
        )
        .map_err(Error::mls)?
        .ok_or(Error::UnknownGroup)
    }

    fn members_of(group: &MlsGroup) -> Result<Vec<MemberInfo>> {
        group
            .members()
            .map(|m| member_info(&m.credential, &m.signature_key, m.index.u32()))
            .collect()
    }

    /// Create a new group with this device as its only member (epoch 0). `group_id` is the
    /// server's `grp_…` id. Activation then adds the other members with [`Device::commit`].
    pub fn create_group(&mut self, group_id: &str) -> Result<Joined> {
        validate_group_name(group_id)?;
        if self.groups.contains(group_id) {
            return Err(Error::GroupExists);
        }
        let group = MlsGroup::new_with_group_id(
            &self.provider,
            &self.signer,
            &create_config(),
            GroupId::from_slice(group_id.as_bytes()),
            self.credential_with_key()?,
        )
        .map_err(Error::mls)?;
        self.groups.insert(group_id.to_owned());
        Ok(Joined {
            group_id: group_id.to_owned(),
            epoch: group.epoch().as_u64(),
            members: Self::members_of(&group)?,
        })
    }

    /// Current members (devices) of a group.
    pub fn members(&self, group_id: &str) -> Result<Vec<MemberInfo>> {
        Self::members_of(&self.load(group_id)?)
    }

    /// Current epoch of a group.
    pub fn epoch(&self, group_id: &str) -> Result<u64> {
        Ok(self.load(group_id)?.epoch().as_u64())
    }

    /// Whether a commit from this device awaits the server's verdict.
    pub fn has_pending_commit(&self, group_id: &str) -> Result<bool> {
        Ok(self.load(group_id)?.pending_commit().is_some())
    }

    /// Safety number over the group's current device list (see [`crate::safety_number`]).
    pub fn safety_number(&self, group_id: &str) -> Result<String> {
        let fps: Vec<[u8; 32]> = self
            .members(group_id)?
            .iter()
            .map(|m| m.fingerprint)
            .collect();
        Ok(safety_number(&fps))
    }

    /// RFC 9420 epoch authenticator: equal on every member iff they share the same epoch state.
    pub fn epoch_authenticator(&self, group_id: &str) -> Result<Vec<u8>> {
        Ok(self
            .load(group_id)?
            .epoch_authenticator()
            .as_slice()
            .to_vec())
    }

    fn aad(&self, kind: MessageKind, group: &MlsGroup, id: &str) -> Result<Vec<u8>> {
        MessageAad {
            kind,
            group_id: group.group_id().as_slice().to_vec(),
            sender_mailbox: self.mailbox.clone(),
            sender_device: self.device_id.clone(),
            epoch: group.epoch().as_u64(),
            client_message_id: id.to_owned(),
        }
        .encode()
    }

    fn resolve(group: &MlsGroup, who: &[DeviceRef]) -> Result<Vec<MemberInfo>> {
        let members = Self::members_of(group)?;
        who.iter()
            .map(|r| {
                members
                    .iter()
                    .find(|m| m.mailbox == r.mailbox && m.device_id == r.device_id)
                    .cloned()
                    .ok_or_else(|| Error::MemberNotFound(format!("{} {}", r.mailbox, r.device_id)))
            })
            .collect()
    }

    /// Build one commit that adds the devices behind `key_packages` and removes `remove`.
    /// An empty commit (no adds, no removes) is a self-update that rotates this device's keys.
    pub fn commit(
        &mut self,
        group_id: &str,
        key_packages: &[Vec<u8>],
        remove: &[DeviceRef],
    ) -> Result<CommitBundle> {
        let mut group = self.load(group_id)?;
        if group.pending_commit().is_some() {
            return Err(Error::PendingCommit);
        }
        if !group.is_active() {
            return Err(Error::NotMember);
        }
        let current = Self::members_of(&group)?;
        let removed = Self::resolve(&group, remove)?;
        if removed
            .iter()
            .any(|m| m.leaf_index == group.own_leaf_index().u32())
        {
            return Err(Error::invalid(
                "a device cannot remove itself; ask another member to commit",
            ));
        }

        // Validate every key package and refuse duplicate devices before touching the group.
        let mut adds = Vec::with_capacity(key_packages.len());
        let mut added = Vec::with_capacity(key_packages.len());
        let mut present: BTreeSet<DeviceRef> = current
            .iter()
            .filter(|m| !removed.iter().any(|r| r.leaf_index == m.leaf_index))
            .map(|m| DeviceRef {
                mailbox: m.mailbox.clone(),
                device_id: m.device_id.clone(),
            })
            .collect();
        for bytes in key_packages {
            let kp = match parse_message(bytes)? {
                MlsMessageBodyIn::KeyPackage(kp) => kp
                    .validate(self.provider.crypto(), ProtocolVersion::Mls10)
                    .map_err(|e| Error::InvalidCredential(format!("key package: {e:?}")))?,
                _ => return Err(Error::Unsupported("expected a key package")),
            };
            let info = describe_key_package(&kp, self.provider.crypto())?;
            if !present.insert(DeviceRef {
                mailbox: info.mailbox.clone(),
                device_id: info.device_id.clone(),
            }) {
                return Err(Error::DuplicateDevice(format!(
                    "{} {}",
                    info.mailbox, info.device_id
                )));
            }
            added.push(MemberInfo {
                mailbox: info.mailbox,
                device_id: info.device_id,
                leaf_index: u32::MAX,
                signature_key: info.signature_key,
                fingerprint: info.fingerprint,
            });
            adds.push(kp);
        }

        let commit_id = new_id("c")?;
        group.set_aad(self.aad(MessageKind::Commit, &group, &commit_id)?);
        let epoch = group.epoch().as_u64();
        let bundle = group
            .commit_builder()
            .propose_adds(adds)
            .propose_removals(removed.iter().map(|m| LeafNodeIndex::new(m.leaf_index)))
            .force_self_update(true)
            .load_psks(self.provider.storage())
            .map_err(Error::mls)?
            .build(
                self.provider.rand(),
                self.provider.crypto(),
                &self.signer,
                |_| true,
            )
            .map_err(Error::mls)?
            .stage_commit(&self.provider)
            .map_err(Error::mls)?;
        let (commit, welcome, _group_info) = bundle.into_messages();
        let commit = serialize(&commit)?;
        let welcome = welcome.as_ref().map(serialize).transpose()?;
        let welcome_recipients = match &welcome {
            Some(w) => welcome_recipients(w)?,
            None => Vec::new(),
        };
        self.pending_commits.insert(
            group_id.to_owned(),
            (Sha256::digest(&commit).into(), commit_id.clone()),
        );
        Ok(CommitBundle {
            group_id: group_id.to_owned(),
            epoch,
            commit_id,
            envelope_signature: self.sign_envelope(&commit)?,
            commit,
            welcome,
            welcome_recipients,
            added,
            removed,
        })
    }

    /// Add devices (one key package per device).
    pub fn add_members(
        &mut self,
        group_id: &str,
        key_packages: &[Vec<u8>],
    ) -> Result<CommitBundle> {
        if key_packages.is_empty() {
            return Err(Error::invalid("no key packages"));
        }
        self.commit(group_id, key_packages, &[])
    }

    /// Remove devices.
    pub fn remove_members(&mut self, group_id: &str, remove: &[DeviceRef]) -> Result<CommitBundle> {
        if remove.is_empty() {
            return Err(Error::invalid("no members to remove"));
        }
        self.commit(group_id, &[], remove)
    }

    /// Remove every device of a mailbox (member removal, ban, offboarding).
    pub fn remove_mailbox(&mut self, group_id: &str, mailbox: &str) -> Result<CommitBundle> {
        let devices: Vec<DeviceRef> = self
            .members(group_id)?
            .into_iter()
            .filter(|m| m.mailbox == mailbox)
            .map(|m| DeviceRef {
                mailbox: m.mailbox,
                device_id: m.device_id,
            })
            .collect();
        if devices.is_empty() {
            return Err(Error::MemberNotFound(mailbox.to_owned()));
        }
        self.commit(group_id, &[], &devices)
    }

    /// Rotate this device's leaf keys (post-compromise security).
    pub fn self_update(&mut self, group_id: &str) -> Result<CommitBundle> {
        self.commit(group_id, &[], &[])
    }

    /// The server accepted this device's pending commit: apply it.
    pub fn confirm_commit(&mut self, group_id: &str) -> Result<CommitOutcome> {
        let mut group = self.load(group_id)?;
        let staged = group.pending_commit().ok_or(Error::NoPendingCommit)?;
        let committer = member_info(
            &group.credential().map_err(Error::mls)?.clone(),
            self.signer.public(),
            group.own_leaf_index().u32(),
        )?;
        let (added, removed, updated) = Self::summarize(&group, staged)?;
        group
            .merge_pending_commit(&self.provider)
            .map_err(Error::mls)?;
        let commit_id = self
            .pending_commits
            .remove(group_id)
            .map(|(_, id)| id)
            .unwrap_or_default();
        let added = Self::with_indices(&group, added)?;
        Ok(CommitOutcome {
            group_id: group_id.to_owned(),
            committer,
            commit_id,
            epoch: group.epoch().as_u64(),
            added,
            removed,
            updated,
            self_removed: false,
        })
    }

    /// The server rejected this device's pending commit (`revision_conflict`/`epoch_conflict`):
    /// drop it, process the winning commit, then rebuild.
    pub fn discard_commit(&mut self, group_id: &str) -> Result<()> {
        let mut group = self.load(group_id)?;
        if group.pending_commit().is_none() {
            return Err(Error::NoPendingCommit);
        }
        group
            .clear_pending_commit(self.provider.storage())
            .map_err(Error::mls)?;
        self.pending_commits.remove(group_id);
        Ok(())
    }

    fn with_indices(group: &MlsGroup, added: Vec<MemberInfo>) -> Result<Vec<MemberInfo>> {
        let members = Self::members_of(group)?;
        Ok(added
            .into_iter()
            .map(|a| {
                members
                    .iter()
                    .find(|m| m.fingerprint == a.fingerprint)
                    .cloned()
                    .unwrap_or(a)
            })
            .collect())
    }

    /// Validate every credential a commit introduces and describe the change. Updated leaves
    /// must keep the exact same identity and signature key (device keys never rotate in place).
    fn summarize(
        group: &MlsGroup,
        staged: &StagedCommit,
    ) -> Result<(Vec<MemberInfo>, Vec<MemberInfo>, Vec<MemberInfo>)> {
        let members = Self::members_of(group)?;
        let removed: Vec<MemberInfo> = staged
            .remove_proposals()
            .map(|p| {
                let idx = p.remove_proposal().removed().u32();
                members
                    .iter()
                    .find(|m| m.leaf_index == idx)
                    .cloned()
                    .ok_or(Error::BindingMismatch("commit removes an unknown leaf"))
            })
            .collect::<Result<_>>()?;
        let mut present: BTreeSet<(String, String)> = members
            .iter()
            .filter(|m| !removed.iter().any(|r| r.leaf_index == m.leaf_index))
            .map(|m| (m.mailbox.clone(), m.device_id.clone()))
            .collect();
        let mut added = Vec::new();
        for p in staged.add_proposals() {
            let info = leaf_member_info(p.add_proposal().key_package().leaf_node(), u32::MAX)?;
            if !present.insert((info.mailbox.clone(), info.device_id.clone())) {
                return Err(Error::DuplicateDevice(format!(
                    "{} {}",
                    info.mailbox, info.device_id
                )));
            }
            added.push(info);
        }
        let mut updated = Vec::new();
        let same_as = |leaf: &LeafNode, idx: u32| -> Result<MemberInfo> {
            let info = leaf_member_info(leaf, idx)?;
            let before = members
                .iter()
                .find(|m| m.leaf_index == idx)
                .ok_or(Error::BindingMismatch("update for unknown leaf"))?;
            if before.mailbox != info.mailbox
                || before.device_id != info.device_id
                || before.signature_key != info.signature_key
            {
                return Err(Error::BindingMismatch(
                    "a leaf update changed the device identity",
                ));
            }
            Ok(info)
        };
        for p in staged.update_proposals() {
            let idx = match p.sender() {
                Sender::Member(i) => i.u32(),
                _ => return Err(Error::Unsupported("update from a non-member")),
            };
            updated.push(same_as(p.update_proposal().leaf_node(), idx)?);
        }
        if let Some(leaf) = staged.update_path_leaf_node() {
            // The committer's own path leaf: find which member it replaces by signature key.
            let idx = members
                .iter()
                .find(|m| m.signature_key == leaf.signature_key().as_slice())
                .map(|m| m.leaf_index)
                .ok_or(Error::BindingMismatch("update path from an unknown device"))?;
            updated.push(same_as(leaf, idx)?);
        }
        if staged.psk_proposals().next().is_some() {
            return Err(Error::Unsupported("PSK proposals"));
        }
        for q in staged.queued_proposals() {
            match q.proposal() {
                Proposal::Add(_) | Proposal::Remove(_) | Proposal::Update(_) => {}
                _ => return Err(Error::Unsupported("proposal type")),
            }
        }
        Ok((added, removed, updated))
    }

    /// Join a group from a Welcome addressed to one of this device's key packages.
    pub fn join_from_welcome(&mut self, welcome: &[u8]) -> Result<Joined> {
        if self.credential.is_none() {
            return Err(Error::NotAttested);
        }
        let welcome = match parse_message(welcome)? {
            MlsMessageBodyIn::Welcome(w) => w,
            _ => return Err(Error::Unsupported("expected a welcome")),
        };
        // Enforce the application's sole suite before the provider sees any
        // attacker-chosen algorithm selection or accesses key-package secrets.
        if welcome.ciphersuite() != CIPHERSUITE {
            return Err(Error::Unsupported("group ciphersuite"));
        }
        let staged = StagedWelcome::new_from_welcome(&self.provider, &join_config(), welcome, None)
            .map_err(Error::mls)?;
        let group_id = group_id_string(staged.group_context().group_id())?;
        if self.groups.contains(&group_id) {
            return Err(Error::GroupExists);
        }
        if staged.group_context().ciphersuite() != CIPHERSUITE {
            return Err(Error::Unsupported("group ciphersuite"));
        }
        // Every leaf must carry a valid mailbox attestation, with no duplicate devices.
        let mut seen = BTreeSet::new();
        for m in staged.members() {
            let info = member_info(&m.credential, &m.signature_key, m.index.u32())?;
            if !seen.insert((info.mailbox, info.device_id.clone())) {
                return Err(Error::DuplicateDevice(info.device_id));
            }
        }
        let group = staged.into_group(&self.provider).map_err(Error::mls)?;
        self.groups.insert(group_id.clone());
        Ok(Joined {
            group_id,
            epoch: group.epoch().as_u64(),
            members: Self::members_of(&group)?,
        })
    }

    fn encrypt_payload(&mut self, group_id: &str, payload: Payload) -> Result<OutgoingMessage> {
        validate_client_message_id(&payload.client_message_id)?;
        let mut group = self.load(group_id)?;
        if !group.is_active() {
            return Err(Error::NotMember);
        }
        let plaintext = payload.encode()?;
        group.set_aad(self.aad(MessageKind::Application, &group, &payload.client_message_id)?);
        let epoch = group.epoch().as_u64();
        let message = group
            .create_message(&self.provider, &self.signer, &plaintext)
            .map_err(Error::mls)?;
        let ciphertext = serialize(&message)?;
        Ok(OutgoingMessage {
            group_id: group_id.to_owned(),
            epoch,
            client_message_id: payload.client_message_id,
            envelope_signature: self.sign_envelope(&ciphertext)?,
            ciphertext,
        })
    }

    /// Encrypt an application message. `client_message_id` must be unique per sender; reuse it
    /// (with a fresh encryption) when retrying after `epoch_conflict`.
    pub fn encrypt_message(
        &mut self,
        group_id: &str,
        client_message_id: &str,
        body: &[u8],
        attachments: &[AttachmentRef],
    ) -> Result<OutgoingMessage> {
        self.encrypt_payload(
            group_id,
            Payload {
                kind: PayloadKind::Message,
                client_message_id: client_message_id.to_owned(),
                body: body.to_vec(),
                attachments: attachments.to_vec(),
            },
        )
    }

    fn is_self_group(&self, group: &MlsGroup) -> Result<bool> {
        Ok(Self::members_of(group)?
            .iter()
            .all(|m| m.mailbox == self.mailbox))
    }

    /// Seal a history archive for this mailbox's other devices. Allowed only in a *self
    /// group* (every member is one of this mailbox's own devices, which the user verified when
    /// adding them). The archive key travels inside the MLS message; upload `sealed_archive`.
    pub fn seal_history(
        &mut self,
        self_group_id: &str,
        client_message_id: &str,
        archive: &[u8],
    ) -> Result<HistoryTransfer> {
        if !self.is_self_group(&self.load(self_group_id)?)? {
            return Err(Error::NotSelfGroup);
        }
        let sealed = seal_attachment(archive)?;
        let message = self.encrypt_payload(
            self_group_id,
            Payload {
                kind: PayloadKind::HistoryOffer,
                client_message_id: client_message_id.to_owned(),
                body: Vec::new(),
                attachments: vec![sealed.reference.clone()],
            },
        )?;
        Ok(HistoryTransfer {
            sealed_archive: sealed.sealed,
            message,
        })
    }

    /// Open a history archive received through [`Incoming::HistoryOffer`].
    pub fn open_history(&self, offer: &HistoryOffer, sealed_archive: &[u8]) -> Result<Vec<u8>> {
        if offer.sender.mailbox != self.mailbox {
            return Err(Error::NotSelfGroup);
        }
        open_attachment(sealed_archive, &offer.archive)
    }

    /// Process one message the server delivered for `group_id`, in server sequence order.
    pub fn process_incoming(&mut self, group_id: &str, message: &[u8]) -> Result<Incoming> {
        let mut group = self.load(group_id)?;
        let digest: [u8; 32] = Sha256::digest(message).into();
        let protocol = match parse_message(message)? {
            MlsMessageBodyIn::PrivateMessage(m) => m,
            MlsMessageBodyIn::PublicMessage(_) => {
                return Err(Error::Unsupported("plaintext group messages"))
            }
            _ => return Err(Error::Unsupported("not a group message")),
        };
        if protocol.group_id().as_slice() != group_id.as_bytes() {
            return Err(Error::BindingMismatch("message is for another group"));
        }
        let (message_epoch, group_epoch) = (protocol.epoch().as_u64(), group.epoch().as_u64());
        if message_epoch < group_epoch {
            return Err(Error::StaleEpoch {
                message_epoch,
                group_epoch,
            });
        }
        if message_epoch > group_epoch {
            return Err(Error::FutureEpoch {
                message_epoch,
                group_epoch,
            });
        }
        let aad = MessageAad::decode(protocol.aad())?;
        if aad.group_id != group_id.as_bytes() || aad.epoch != message_epoch {
            return Err(Error::BindingMismatch(
                "authenticated data differs from MLS header",
            ));
        }
        let is_commit = protocol.content_type() == ContentType::Commit;
        if is_commit != (aad.kind == MessageKind::Commit) {
            return Err(Error::BindingMismatch(
                "content type differs from authenticated data",
            ));
        }
        if protocol.content_type() == ContentType::Proposal {
            return Err(Error::Unsupported("standalone proposals"));
        }

        let processed = group
            .process_message(&self.provider, ProtocolMessage::from(protocol))
            .map_err(|e| match e {
                ProcessMessageError::GroupStateError(MlsGroupStateError::UseAfterEviction) => {
                    Error::NotMember
                }
                ProcessMessageError::ValidationError(ValidationError::UnableToDecrypt(_)) => {
                    Error::DecryptionFailed
                }
                other => Error::mls(other),
            })?;

        if let ProcessedMessageContent::OwnPrivateMessage = processed.content() {
            if is_commit && self.pending_commits.get(group_id).map(|(d, _)| d) == Some(&digest) {
                return self
                    .confirm_commit(group_id)
                    .map(Incoming::OwnCommitConfirmed);
            }
            return Ok(Incoming::Own);
        }

        let sender_index = match processed.sender() {
            Sender::Member(i) => *i,
            _ => return Err(Error::Unsupported("non-member sender")),
        };
        let sender_key = group
            .member_at(sender_index)
            .map(|m| m.signature_key)
            .ok_or(Error::BindingMismatch("sender is not in the group"))?;
        let sender = member_info(processed.credential(), &sender_key, sender_index.u32())?;
        if sender.mailbox != aad.sender_mailbox || sender.device_id != aad.sender_device {
            return Err(Error::BindingMismatch(
                "authenticated data names a different sender",
            ));
        }

        match processed.into_content() {
            ProcessedMessageContent::ApplicationMessage(app) => {
                let payload = Payload::decode(&app.into_bytes())?;
                if payload.client_message_id != aad.client_message_id {
                    return Err(Error::BindingMismatch(
                        "client message id differs from authenticated data",
                    ));
                }
                match payload.kind {
                    PayloadKind::Message => Ok(Incoming::Message(ReceivedMessage {
                        group_id: group_id.to_owned(),
                        sender,
                        epoch: message_epoch,
                        client_message_id: payload.client_message_id,
                        body: payload.body,
                        attachments: payload.attachments,
                    })),
                    PayloadKind::HistoryOffer => {
                        if sender.mailbox != self.mailbox
                            || !self.is_self_group(&group)?
                            || payload.attachments.len() != 1
                        {
                            return Err(Error::NotSelfGroup);
                        }
                        Ok(Incoming::HistoryOffer(HistoryOffer {
                            group_id: group_id.to_owned(),
                            sender,
                            client_message_id: payload.client_message_id,
                            archive: payload.attachments[0].clone(),
                        }))
                    }
                }
            }
            ProcessedMessageContent::StagedCommitMessage(staged) => {
                let (added, removed, updated) = Self::summarize(&group, &staged)?;
                let self_removed = staged.self_removed();
                // A commit the server accepted for this epoch supersedes any commit of ours.
                if group.pending_commit().is_some() {
                    group
                        .clear_pending_commit(self.provider.storage())
                        .map_err(Error::mls)?;
                    self.pending_commits.remove(group_id);
                }
                group
                    .merge_staged_commit(&self.provider, *staged)
                    .map_err(Error::mls)?;
                let epoch = group.epoch().as_u64();
                let added = if self_removed {
                    added
                } else {
                    Self::with_indices(&group, added)?
                };
                if self_removed {
                    self.forget_group(group_id)?;
                }
                Ok(Incoming::Commit(CommitOutcome {
                    group_id: group_id.to_owned(),
                    committer: sender,
                    commit_id: aad.client_message_id,
                    epoch,
                    added,
                    removed,
                    updated,
                    self_removed,
                }))
            }
            _ => Err(Error::Unsupported("message type")),
        }
    }

    /// Erase all local state for a group (after leaving, removal or group deletion).
    pub fn forget_group(&mut self, group_id: &str) -> Result<()> {
        if let Ok(mut group) = self.load(group_id) {
            group.delete(self.provider.storage()).map_err(Error::mls)?;
        }
        self.groups.remove(group_id);
        self.pending_commits.remove(group_id);
        Ok(())
    }
}
