//! Profile pictures (SDS §16): upload, processing, visibility and serving.
//!
//! # Processing
//!
//! An uploaded picture is untrusted input to an image decoder, so decoding is bounded every way
//! the decoder allows — compressed bytes, dimensions, decoded pixels and allocation — and, in
//! production, it runs in a separate short-lived process (`pigeonpost-postbox --avatar-worker`)
//! under `setrlimit` caps on address space, CPU time and file writes, killed if it overruns its
//! wall-clock budget. A decoder bug therefore costs one worker, not the postbox. The decoders are
//! the pure-Rust ones from the `image` crate (JPEG, PNG, WebP); HEIC is converted on the client.
//!
//! The output never contains a byte of the input's container: pixels are decoded, oriented,
//! cropped to a square, and re-encoded as WebP at 64/128/256/512 px plus a 256 px JPEG fallback.
//! Every piece of metadata (EXIF, GPS, ICC, XMP, comments) is left behind with the container.
//!
//! # Serving
//!
//! Every blob URL names the picture's opaque id and its version and is authorized on every
//! request. A visibility change bumps the version, so earlier URLs stop working here (copies
//! already downloaded, or cached publicly while the picture was public, cannot be recalled).

use crate::capabilities;
use crate::filtering::{feature_on, mailbox_for};
use crate::policy::{authorize, Action, Actor, Facts, Resource};
use crate::store::{self, AvatarRow, NewAvatar, SetAvatar, Visibility};
use crate::{bearer, now_unix, principal_for_token, ApiError, AppState, Principal};
use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;
use std::collections::BTreeMap;
use std::sync::Arc;

/// Largest upload accepted (SDS §16.2).
pub(crate) const MAX_UPLOAD: usize = 10 * 1024 * 1024;
/// Largest side and pixel count decoded. 40 MP covers every phone camera.
const MAX_SIDE: u32 = 8_192;
const MAX_PIXELS: u64 = 40_000_000;
/// Decoder allocation ceiling: a 40 MP RGBA frame plus working space.
const MAX_ALLOC: u64 = 256 * 1024 * 1024;
/// Each stored variant is at most this big.
pub(crate) const VARIANT_MAX: usize = 60 * 1024;
/// The WebP sizes, largest first.
pub(crate) const SIZES: [u32; 4] = [512, 256, 128, 64];
/// The JPEG fallback's size.
const JPEG_SIZE: u32 = 256;
/// Wall-clock budget for one worker.
#[cfg_attr(not(unix), allow(dead_code))]
const WORKER_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);
/// Reports one mailbox may file per day.
const REPORTS_PER_DAY: usize = 20;
/// Addresses per bulk lookup.
const BULK_MAX: usize = 100;

/// Why a picture was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AvatarError {
    TooLarge,
    Unsupported,
    Bad(&'static str),
    Internal,
}

impl AvatarError {
    fn code(self) -> u8 {
        match self {
            AvatarError::TooLarge => 1,
            AvatarError::Unsupported => 2,
            AvatarError::Bad(_) => 3,
            AvatarError::Internal => 4,
        }
    }
    #[cfg_attr(not(unix), allow(dead_code))]
    fn from_code(code: u8) -> Self {
        match code {
            1 => AvatarError::TooLarge,
            2 => AvatarError::Unsupported,
            3 => AvatarError::Bad("the image could not be used"),
            _ => AvatarError::Internal,
        }
    }
    fn into_api(self) -> ApiError {
        match self {
            AvatarError::TooLarge => ApiError::new(
                StatusCode::PAYLOAD_TOO_LARGE,
                "too_large",
                "pictures are limited to 10 MB",
            ),
            AvatarError::Unsupported => ApiError::new(
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "unsupported_image",
                "send a JPEG, PNG or WebP picture (convert HEIC on the device first)",
            ),
            AvatarError::Bad(why) => {
                ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "bad_image", why)
            }
            AvatarError::Internal => ApiError::server("avatar_processing_failed"),
        }
    }
}

/// A crop rectangle in source pixels, after orientation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Crop {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

impl Crop {
    pub(crate) fn parse(raw: &str) -> Option<Crop> {
        let parts: Vec<u32> = raw
            .split(',')
            .map(|p| p.trim().parse().ok())
            .collect::<Option<_>>()?;
        match parts[..] {
            [x, y, w, h] if w > 0 && h > 0 => Some(Crop { x, y, w, h }),
            _ => None,
        }
    }
}

/// The square actually taken: `(x, y, side)`, clamped inside the image.
pub(crate) fn square(width: u32, height: u32, crop: Option<Crop>) -> (u32, u32, u32) {
    let max_side = width.min(height);
    match crop {
        None => ((width - max_side) / 2, (height - max_side) / 2, max_side),
        Some(c) => {
            let side = c.w.min(c.h).clamp(1, max_side);
            (c.x.min(width - side), c.y.min(height - side), side)
        }
    }
}

/// The encoded variants.
#[derive(Debug)]
pub(crate) struct Processed {
    /// `(size, webp bytes)`, largest first.
    pub webp: Vec<(u32, Vec<u8>)>,
    pub jpeg: Vec<u8>,
}

fn webp_under_limit(rgba: &image::RgbaImage, alpha: bool) -> Result<Vec<u8>, AvatarError> {
    let (w, h) = rgba.dimensions();
    let rgb;
    let encoder = if alpha {
        webp::Encoder::from_rgba(rgba.as_raw(), w, h)
    } else {
        rgb = image::DynamicImage::ImageRgba8(rgba.clone()).to_rgb8();
        webp::Encoder::from_rgb(rgb.as_raw(), w, h)
    };
    for quality in [82.0, 70.0, 55.0, 40.0, 25.0] {
        let out = encoder
            .encode_simple(false, quality)
            .map_err(|_| AvatarError::Internal)?;
        if out.len() <= VARIANT_MAX {
            return Ok(out.to_vec());
        }
    }
    Err(AvatarError::Bad("the picture is too detailed to store"))
}

fn jpeg_under_limit(rgba: &image::RgbaImage) -> Result<Vec<u8>, AvatarError> {
    // JPEG has no alpha: flatten onto white, which is what a transparent picture looks like on
    // the light theme every fallback viewer starts in.
    let mut flat = image::RgbImage::new(rgba.width(), rgba.height());
    for (dst, src) in flat.pixels_mut().zip(rgba.pixels()) {
        let a = u16::from(src[3]);
        for i in 0..3 {
            dst[i] = ((u16::from(src[i]) * a + 255 * (255 - a)) / 255) as u8;
        }
    }
    for quality in [85, 70, 55, 40] {
        let mut out = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, quality)
            .encode_image(&flat)
            .map_err(|_| AvatarError::Internal)?;
        if out.len() <= VARIANT_MAX {
            return Ok(out);
        }
    }
    Err(AvatarError::Bad("the picture is too detailed to store"))
}

/// Decode, orient, crop, resize and re-encode one picture. Pure, bounded, in-process: the worker
/// process calls this, and so do tests.
pub(crate) fn process(bytes: &[u8], crop: Option<Crop>) -> Result<Processed, AvatarError> {
    use image::{DynamicImage, ImageDecoder, ImageFormat, ImageReader, Limits};
    if bytes.len() > MAX_UPLOAD {
        return Err(AvatarError::TooLarge);
    }
    let format = image::guess_format(bytes).map_err(|_| AvatarError::Unsupported)?;
    if !matches!(
        format,
        ImageFormat::Jpeg | ImageFormat::Png | ImageFormat::WebP
    ) {
        return Err(AvatarError::Unsupported);
    }
    let mut reader = ImageReader::with_format(std::io::Cursor::new(bytes), format);
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_SIDE);
    limits.max_image_height = Some(MAX_SIDE);
    limits.max_alloc = Some(MAX_ALLOC);
    reader.limits(limits);
    let mut decoder = reader
        .into_decoder()
        .map_err(|_| AvatarError::Bad("the picture could not be read"))?;
    let (w, h) = decoder.dimensions();
    if w == 0 || h == 0 || u64::from(w) * u64::from(h) > MAX_PIXELS {
        return Err(AvatarError::Bad("the picture has too many pixels"));
    }
    let orientation = decoder
        .orientation()
        .unwrap_or(image::metadata::Orientation::NoTransforms);
    let mut img = DynamicImage::from_decoder(decoder)
        .map_err(|_| AvatarError::Bad("the picture could not be decoded"))?;
    img.apply_orientation(orientation);
    let (x, y, side) = square(img.width(), img.height(), crop);
    let cropped = img.crop_imm(x, y, side, side).to_rgba8();
    drop(img);
    let alpha = cropped.pixels().any(|p| p[3] < 255);
    let filter = image::imageops::FilterType::Lanczos3;
    // Each size from the one above it: a 4096 px square shrinks once at full cost, then cheaply.
    let mut webp = Vec::with_capacity(SIZES.len());
    let mut jpeg = None;
    let mut source = cropped;
    for size in SIZES {
        let resized = image::imageops::resize(&source, size, size, filter);
        webp.push((size, webp_under_limit(&resized, alpha)?));
        if size == JPEG_SIZE {
            jpeg = Some(jpeg_under_limit(&resized)?);
        }
        source = resized;
    }
    Ok(Processed {
        webp,
        jpeg: jpeg.ok_or(AvatarError::Internal)?,
    })
}

// ---- the worker process --------------------------------------------------------------------------

/// Wire format, worker → parent: `0` then for each variant `size:u16le len:u32le bytes`, with size 0
/// for the JPEG; or `1` and one error code byte.
fn encode_output(result: &Result<Processed, AvatarError>) -> Vec<u8> {
    match result {
        Ok(p) => {
            let mut out = vec![0u8];
            for (size, bytes) in p
                .webp
                .iter()
                .map(|(s, b)| (*s as u16, b))
                .chain(std::iter::once((0u16, &p.jpeg)))
            {
                out.extend_from_slice(&size.to_le_bytes());
                out.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
                out.extend_from_slice(bytes);
            }
            out
        }
        Err(e) => vec![1u8, e.code()],
    }
}

#[cfg_attr(not(unix), allow(dead_code))]
fn decode_output(raw: &[u8]) -> Result<Processed, AvatarError> {
    match raw.first() {
        Some(1) => return Err(AvatarError::from_code(*raw.get(1).unwrap_or(&4))),
        Some(0) => {}
        _ => return Err(AvatarError::Internal),
    }
    let mut at = 1;
    let mut webp = Vec::new();
    let mut jpeg = None;
    while at < raw.len() {
        let head = raw.get(at..at + 6).ok_or(AvatarError::Internal)?;
        let size = u16::from_le_bytes([head[0], head[1]]);
        let len = u32::from_le_bytes([head[2], head[3], head[4], head[5]]) as usize;
        at += 6;
        if len > VARIANT_MAX {
            return Err(AvatarError::Internal);
        }
        let bytes = raw.get(at..at + len).ok_or(AvatarError::Internal)?.to_vec();
        at += len;
        match size {
            0 => jpeg = Some(bytes),
            s if SIZES.contains(&u32::from(s)) => webp.push((u32::from(s), bytes)),
            _ => return Err(AvatarError::Internal),
        }
    }
    if webp.len() != SIZES.len() {
        return Err(AvatarError::Internal);
    }
    Ok(Processed {
        webp,
        jpeg: jpeg.ok_or(AvatarError::Internal)?,
    })
}

/// `pigeonpost-postbox --avatar-worker [x,y,w,h]`: read one picture from stdin, write the variants
/// to stdout, exit. Runs under the limits the parent set before `exec`.
pub(crate) fn run_worker(args: &[String]) -> i32 {
    use std::io::{Read, Write};
    let crop = args
        .iter()
        .skip_while(|a| *a != "--avatar-worker")
        .nth(1)
        .and_then(|c| Crop::parse(c));
    let mut input = Vec::new();
    let read = std::io::stdin()
        .take(MAX_UPLOAD as u64 + 1)
        .read_to_end(&mut input);
    let result = match read {
        Ok(_) => process(&input, crop),
        Err(_) => Err(AvatarError::Internal),
    };
    let out = encode_output(&result);
    let mut stdout = std::io::stdout().lock();
    if stdout.write_all(&out).and_then(|_| stdout.flush()).is_err() {
        return 1;
    }
    0
}

/// Where decoding happens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Isolation {
    /// A child process under resource limits (production on Unix).
    Process,
    /// In this process (tests, and platforms without `setrlimit`).
    Inline,
}

/// Runs picture processing with bounded concurrency.
pub(crate) struct Processor {
    isolation: Isolation,
    slots: tokio::sync::Semaphore,
}

impl Processor {
    pub(crate) fn new(isolation: Isolation) -> Self {
        Processor {
            isolation,
            slots: tokio::sync::Semaphore::new(2),
        }
    }

    /// `POSTBOX_AVATAR_WORKER=inline` opts out of the child process; Unix defaults to it.
    pub(crate) fn from_env() -> Self {
        let inline = std::env::var("POSTBOX_AVATAR_WORKER").is_ok_and(|v| v.trim() == "inline");
        Self::new(if inline || !cfg!(unix) {
            Isolation::Inline
        } else {
            Isolation::Process
        })
    }

    pub(crate) async fn run(
        &self,
        bytes: Bytes,
        crop: Option<Crop>,
    ) -> Result<Processed, AvatarError> {
        let _slot = self
            .slots
            .acquire()
            .await
            .map_err(|_| AvatarError::Internal)?;
        match self.isolation {
            Isolation::Inline => tokio::task::spawn_blocking(move || process(&bytes, crop))
                .await
                .map_err(|_| AvatarError::Internal)?,
            Isolation::Process => run_in_child(bytes, crop).await,
        }
    }
}

#[cfg(unix)]
async fn run_in_child(bytes: Bytes, crop: Option<Crop>) -> Result<Processed, AvatarError> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let exe = std::env::current_exe().map_err(|_| AvatarError::Internal)?;
    let mut cmd = tokio::process::Command::new(exe);
    cmd.arg("--avatar-worker");
    if let Some(c) = crop {
        cmd.arg(format!("{},{},{},{}", c.x, c.y, c.w, c.h));
    }
    cmd.env_clear()
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    // SAFETY: only async-signal-safe `setrlimit` calls run between fork and exec.
    unsafe {
        cmd.pre_exec(|| {
            let cap = |resource, value: libc::rlim_t| {
                let lim = libc::rlimit {
                    rlim_cur: value,
                    rlim_max: value,
                };
                if libc::setrlimit(resource, &lim) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            };
            // Address space is the decoder's memory ceiling. Linux enforces it; macOS (a
            // development host only) refuses to lower it, so it is not asked there.
            #[cfg(target_os = "linux")]
            cap(libc::RLIMIT_AS, 1024 * 1024 * 1024)?;
            cap(libc::RLIMIT_CPU, 15)?;
            cap(libc::RLIMIT_FSIZE, 0)?;
            cap(libc::RLIMIT_CORE, 0)?;
            Ok(())
        });
    }
    let mut child = cmd.spawn().map_err(|e| {
        tracing::error!(error = %e, "avatar worker failed to start");
        AvatarError::Internal
    })?;
    let mut stdin = child.stdin.take().ok_or(AvatarError::Internal)?;
    let mut stdout = child.stdout.take().ok_or(AvatarError::Internal)?;
    let work = async move {
        let writer = async move {
            let _ = stdin.write_all(&bytes).await;
            drop(stdin);
        };
        let reader = async move {
            let mut out = Vec::new();
            (&mut stdout)
                .take((SIZES.len() as u64 + 1) * (VARIANT_MAX as u64 + 6) + 2)
                .read_to_end(&mut out)
                .await
                .map(|_| out)
        };
        let ((), out) = tokio::join!(writer, reader);
        out
    };
    let out = match tokio::time::timeout(WORKER_TIMEOUT, work).await {
        Ok(Ok(out)) => out,
        Ok(Err(_)) => return Err(AvatarError::Internal),
        Err(_) => {
            tracing::warn!("avatar worker timed out and was killed");
            let _ = child.kill().await;
            return Err(AvatarError::Bad("the picture took too long to process"));
        }
    };
    let status = child.wait().await.map_err(|_| AvatarError::Internal)?;
    if !status.success() {
        // A decoder that hit a resource limit dies by signal; that is a bad picture, not an outage.
        tracing::warn!(?status, "avatar worker exited abnormally");
        return Err(AvatarError::Bad("the picture could not be processed"));
    }
    decode_output(&out)
}

#[cfg(not(unix))]
async fn run_in_child(bytes: Bytes, crop: Option<Crop>) -> Result<Processed, AvatarError> {
    tokio::task::spawn_blocking(move || process(&bytes, crop))
        .await
        .map_err(|_| AvatarError::Internal)?
}

// ---- views ---------------------------------------------------------------------------------------

fn rand_id() -> String {
    format!("av_{}", crate::rand_hex(13))
}

fn urls(row: &AvatarRow) -> serde_json::Value {
    let base = format!("/v1/avatar-blobs/{}/{}", row.id, row.version);
    let mut map = serde_json::Map::new();
    for size in SIZES.iter().rev() {
        map.insert(size.to_string(), json!(format!("{base}/{size}.webp")));
    }
    map.insert("jpeg".into(), json!(format!("{base}/{JPEG_SIZE}.jpg")));
    serde_json::Value::Object(map)
}

fn public_json(row: &AvatarRow) -> serde_json::Value {
    json!({ "id": row.id, "version": row.version, "hash": row.sha256, "urls": urls(row) })
}

fn own_json(row: &AvatarRow) -> serde_json::Value {
    json!({
        "id": row.id,
        "version": row.version,
        "visibility": row.visibility.as_str(),
        "hash": row.sha256,
        "urls": urls(row),
        "created_at": row.created_at,
    })
}

/// Who is looking: a principal (with the mailboxes it can act as) or nobody.
struct Viewer {
    principal: Option<Principal>,
    /// `(address, handle)` of the viewer's mailboxes.
    mailboxes: Vec<(String, Option<String>)>,
}

async fn viewer(state: &AppState, headers: &HeaderMap) -> Result<Viewer, ApiError> {
    let Some(token) = bearer(headers) else {
        return Ok(Viewer {
            principal: None,
            mailboxes: Vec::new(),
        });
    };
    let principal = principal_for_token(state, Some(token)).await?;
    let mailboxes = match &principal {
        Principal::Identity(id) => vec![(id.address.clone(), id.handle.clone())],
        Principal::Account(account, _) => state
            .store
            .list_by_account(account.clone())
            .await
            .map_err(|_| ApiError::server("store_error"))?
            .into_iter()
            .take(50)
            .map(|m| (m.address, m.handle))
            .collect(),
    };
    Ok(Viewer {
        principal: Some(principal),
        mailboxes,
    })
}

/// Whether `viewer` may see `row`, owned by `owner`.
async fn can_view(
    state: &AppState,
    viewer: &Viewer,
    owner: &store::StoredIdentity,
    row: &AvatarRow,
) -> bool {
    let mut known = false;
    if row.visibility == Visibility::Known {
        for (address, handle) in &viewer.mailboxes {
            if address == &owner.address {
                continue;
            }
            if let Ok(rel) = state
                .store
                .relationship(owner.address.clone(), address.clone(), handle.clone())
                .await
            {
                if rel.known() {
                    known = true;
                    break;
                }
            }
        }
    }
    let actor = match &viewer.principal {
        Some(p) => Actor::Principal(p),
        None => Actor::Anonymous,
    };
    authorize(
        &actor,
        Action::ViewAvatar,
        &Resource::Avatar {
            visibility: row.visibility,
            owner_account: owner.account_id.as_deref(),
            owner_address: &owner.address,
            viewer_known: known,
        },
        &Facts {
            now: now_unix(),
            ..Facts::default()
        },
    )
    .is_ok()
}

/// One address's picture as `viewer` may see it. `None` for no picture, not visible, no such
/// mailbox, or the feature being off — all the same answer.
async fn visible_avatar(
    state: &AppState,
    viewer: &Viewer,
    name: &str,
) -> Result<Option<AvatarRow>, ApiError> {
    let Some(owner) = crate::resolve_recipient(state, name.trim()).await? else {
        return Ok(None);
    };
    if !feature_on(state, owner.account_id.as_deref(), capabilities::AVATARS).await {
        return Ok(None);
    }
    let Some(row) = state
        .store
        .avatar_of(owner.address.clone())
        .await
        .map_err(|_| ApiError::server("store_error"))?
    else {
        return Ok(None);
    };
    Ok(can_view(state, viewer, &owner, &row).await.then_some(row))
}

/// Tell the owner, and everyone who corresponds with them, that the picture changed. Each
/// recipient learns only what it may see: a picture it may not see is announced as none.
async fn announce(state: &AppState, owner: &store::StoredIdentity) {
    let row = state
        .store
        .avatar_of(owner.address.clone())
        .await
        .ok()
        .flatten();
    let now = now_unix();
    let data = |visible: bool| {
        json!({
            "address": owner.address,
            "handle": owner.handle,
            "avatar_id": row.as_ref().filter(|_| visible).map(|r| r.id.clone()),
            "version": row.as_ref().filter(|_| visible).map(|r| r.version),
        })
    };
    let mut sent = std::collections::BTreeSet::new();
    if let Some(account) = owner.account_id.as_deref() {
        if state
            .store
            .record_account_event(
                account.to_string(),
                "profile.avatar".into(),
                Some(owner.address.clone()),
                data(true),
                now,
            )
            .await
            .is_ok()
        {
            state.event_hub.wake(account);
        }
        sent.insert(account.to_string());
    }
    let audience = state
        .store
        .avatar_audience(owner.address.clone())
        .await
        .unwrap_or_default();
    for (address, handle, account) in audience {
        if !sent.insert(account.clone()) {
            continue;
        }
        let visible = match &row {
            None => false,
            Some(r) => match r.visibility {
                Visibility::Everyone => true,
                Visibility::Nobody => false,
                Visibility::Known => state
                    .store
                    .relationship(owner.address.clone(), address.clone(), handle)
                    .await
                    .is_ok_and(|rel| rel.known()),
            },
        };
        if state
            .store
            .record_account_event(
                account.clone(),
                "profile.avatar".into(),
                Some(address),
                data(visible),
                now,
            )
            .await
            .is_ok()
        {
            state.event_hub.wake(&account);
        }
    }
}

fn avatars_off() -> ApiError {
    ApiError::new(
        StatusCode::NOT_FOUND,
        "feature_disabled",
        "profile pictures are not enabled for this account",
    )
}

fn storage(state: &AppState) -> Result<&Arc<crate::blobs::Blobs>, ApiError> {
    state.avatar_blobs.as_ref().ok_or_else(|| {
        ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "avatars_unavailable",
            "this postbox has nowhere to keep pictures",
        )
    })
}

fn release(state: &AppState, digests: &[String]) {
    if let Some(blobs) = state.avatar_blobs.as_ref() {
        for d in digests {
            blobs.remove(d);
        }
    }
}

// ---- member routes -------------------------------------------------------------------------------

#[derive(serde::Deserialize)]
pub(crate) struct UploadQuery {
    #[serde(default)]
    identity: Option<String>,
    #[serde(default)]
    crop: Option<String>,
    #[serde(default)]
    visibility: Option<String>,
    #[serde(default)]
    all: Option<String>,
}

/// `PUT /v1/profile/avatar` — set the picture from the raw image in the body.
pub(crate) async fn upload(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<UploadQuery>,
    body: Bytes,
) -> Response {
    match do_upload(&state, &headers, q, body).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => e.into_response(),
    }
}

async fn do_upload(
    state: &AppState,
    headers: &HeaderMap,
    q: UploadQuery,
    body: Bytes,
) -> Result<serde_json::Value, ApiError> {
    let (principal, me) = mailbox_for(
        state,
        headers,
        q.identity.as_deref(),
        Action::ManageOwnMailbox,
    )
    .await?;
    if !feature_on(state, me.account_id.as_deref(), capabilities::AVATARS).await {
        return Err(avatars_off());
    }
    let blobs = storage(state)?.clone();
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if !(content_type.starts_with("image/") || content_type == "application/octet-stream") {
        return Err(AvatarError::Unsupported.into_api());
    }
    if body.len() > MAX_UPLOAD {
        return Err(AvatarError::TooLarge.into_api());
    }
    if body.is_empty() {
        return Err(AvatarError::Bad("the body was empty").into_api());
    }
    let crop = match q.crop.as_deref() {
        None | Some("") => None,
        Some(raw) => Some(
            Crop::parse(raw)
                .ok_or_else(|| ApiError::bad("bad_request", "crop must be x,y,w,h in pixels"))?,
        ),
    };
    let visibility = match q.visibility.as_deref() {
        None | Some("") => Visibility::Known,
        Some(v) => Visibility::parse(v).ok_or_else(|| {
            ApiError::bad(
                "bad_request",
                "visibility must be everyone, known or nobody",
            )
        })?,
    };
    let all = matches!(q.all.as_deref(), Some("1" | "true"));
    let targets: Vec<store::StoredIdentity> = match (&principal, all) {
        (Principal::Account(account, _), true) => {
            let mut out = Vec::new();
            for m in state
                .store
                .list_by_account(account.clone())
                .await
                .map_err(|_| ApiError::server("store_error"))?
                .into_iter()
                .take(50)
            {
                if let Ok(Some(id)) = state.store.get(m.address).await {
                    out.push(id);
                }
            }
            out
        }
        _ => vec![me.clone()],
    };

    let processed = state
        .avatar_processor
        .run(body, crop)
        .await
        .map_err(AvatarError::into_api)?;
    let mut variants = BTreeMap::new();
    let mut hash = String::new();
    for (size, bytes) in &processed.webp {
        let digest = blobs.put(bytes).map_err(|e| {
            tracing::error!(error = %e, "avatar blob write failed");
            ApiError::server("storage_error")
        })?;
        if *size == SIZES[0] {
            hash = digest.clone();
        }
        variants.insert(size.to_string(), digest);
    }
    let jpeg = blobs
        .put(&processed.jpeg)
        .map_err(|_| ApiError::server("storage_error"))?;
    variants.insert("jpeg".into(), jpeg);

    let now = now_unix();
    let mut stored = Vec::new();
    let mut limited = Vec::new();
    for target in &targets {
        let result = state
            .store
            .set_avatar(
                target.address.clone(),
                NewAvatar {
                    id: rand_id(),
                    sha256: hash.clone(),
                    variants: variants.clone(),
                    visibility,
                },
                now,
            )
            .await
            .map_err(|_| ApiError::server("store_error"))?;
        match result {
            SetAvatar::Stored { row, orphaned } => {
                release(state, &orphaned);
                announce(state, target).await;
                stored.push(json!({ "address": target.address, "avatar": own_json(&row) }));
            }
            SetAvatar::RateLimited => limited.push(target.address.clone()),
        }
    }
    tracing::info!(address = %me.address, mailboxes = stored.len(), limited = limited.len(), "avatar set");
    if stored.is_empty() {
        // Nothing references the fresh blobs; they are reclaimed on the next identical upload's
        // dedup or left tiny and bounded by the same rate limit that refused this one.
        return Err(ApiError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "avatar_rate_limited",
            "a picture can be changed 5 times a day",
        )
        .with_retry_after(3_600));
    }
    if all {
        Ok(json!({ "avatars": stored, "rate_limited": limited }))
    } else {
        Ok(json!({ "avatar": stored[0]["avatar"] }))
    }
}

#[derive(serde::Deserialize)]
pub(crate) struct IdentityQuery {
    #[serde(default)]
    identity: Option<String>,
}

/// `GET /v1/profile/avatar` — the acting mailbox's own picture, with its visibility.
pub(crate) async fn get_own(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<IdentityQuery>,
) -> Response {
    let me = match mailbox_for(
        &state,
        &headers,
        q.identity.as_deref(),
        Action::ManageOwnMailbox,
    )
    .await
    {
        Ok((_, me)) => me,
        Err(e) => return e.into_response(),
    };
    if !feature_on(&state, me.account_id.as_deref(), capabilities::AVATARS).await {
        return avatars_off().into_response();
    }
    match state.store.avatar_of(me.address.clone()).await {
        Ok(row) => Json(json!({ "avatar": row.as_ref().map(own_json) })).into_response(),
        Err(_) => ApiError::server("store_error").into_response(),
    }
}

#[derive(serde::Deserialize)]
pub(crate) struct VisibilityReq {
    visibility: String,
    #[serde(default)]
    identity: Option<String>,
}

/// `PATCH /v1/profile/avatar` — change who may see the picture.
pub(crate) async fn set_visibility(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<VisibilityReq>,
) -> Response {
    let me = match mailbox_for(
        &state,
        &headers,
        req.identity.as_deref(),
        Action::ManageOwnMailbox,
    )
    .await
    {
        Ok((_, me)) => me,
        Err(e) => return e.into_response(),
    };
    if !feature_on(&state, me.account_id.as_deref(), capabilities::AVATARS).await {
        return avatars_off().into_response();
    }
    let Some(visibility) = Visibility::parse(&req.visibility) else {
        return ApiError::bad(
            "bad_request",
            "visibility must be everyone, known or nobody",
        )
        .into_response();
    };
    match state
        .store
        .set_avatar_visibility(me.address.clone(), visibility)
        .await
    {
        Ok(Some(row)) => {
            announce(&state, &me).await;
            Json(json!({ "avatar": own_json(&row) })).into_response()
        }
        Ok(None) => ApiError::new(StatusCode::NOT_FOUND, "not_found", "no picture to change")
            .into_response(),
        Err(_) => ApiError::server("store_error").into_response(),
    }
}

/// `DELETE /v1/profile/avatar` — back to initials.
pub(crate) async fn remove(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<IdentityQuery>,
) -> Response {
    let me = match mailbox_for(
        &state,
        &headers,
        q.identity.as_deref(),
        Action::ManageOwnMailbox,
    )
    .await
    {
        Ok((_, me)) => me,
        Err(e) => return e.into_response(),
    };
    if !feature_on(&state, me.account_id.as_deref(), capabilities::AVATARS).await {
        return avatars_off().into_response();
    }
    match state
        .store
        .remove_avatar(me.address.clone(), "removed", now_unix())
        .await
    {
        Ok((had, orphaned)) => {
            release(&state, &orphaned);
            if had {
                announce(&state, &me).await;
            }
            Json(json!({ "removed": had })).into_response()
        }
        Err(_) => ApiError::server("store_error").into_response(),
    }
}

/// `GET /v1/avatars/{address}` — one mailbox's picture, if the caller may see it.
pub(crate) async fn lookup_one(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(address): Path<String>,
) -> Response {
    let viewer = match viewer(&state, &headers).await {
        Ok(v) => v,
        Err(e) => return e.into_response(),
    };
    let address = if address.starts_with('/') {
        address
    } else {
        format!("/{address}")
    };
    if address.len() > 300 {
        return ApiError::bad("bad_request", "address too long").into_response();
    }
    match visible_avatar(&state, &viewer, &address).await {
        Ok(row) => Json(json!({ "address": address, "avatar": row.as_ref().map(public_json) }))
            .into_response(),
        Err(e) => e.into_response(),
    }
}

#[derive(serde::Deserialize)]
pub(crate) struct BulkQuery {
    addresses: String,
}

/// `GET /v1/avatars?addresses=a,b` — many at once, each decided on its own.
pub(crate) async fn lookup_many(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<BulkQuery>,
) -> Response {
    let viewer = match viewer(&state, &headers).await {
        Ok(v) => v,
        Err(e) => return e.into_response(),
    };
    let names: Vec<&str> = q
        .addresses
        .split(',')
        .map(str::trim)
        .filter(|a| !a.is_empty())
        .collect();
    if names.len() > BULK_MAX || names.iter().any(|a| a.len() > 300) {
        return ApiError::bad("bad_request", "at most 100 addresses per request").into_response();
    }
    let mut out = serde_json::Map::new();
    for name in names {
        if out.contains_key(name) {
            continue;
        }
        let row = match visible_avatar(&state, &viewer, name).await {
            Ok(row) => row,
            Err(e) => return e.into_response(),
        };
        out.insert(name.to_string(), row.as_ref().map(public_json).into());
    }
    Json(json!({ "avatars": out })).into_response()
}

/// `GET /v1/avatar-blobs/{id}/{version}/{file}` — the image bytes, authorized every time.
pub(crate) async fn blob(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((id, version, file)): Path<(String, i64, String)>,
) -> Response {
    let not_found = || ApiError::new(StatusCode::NOT_FOUND, "not_found", "no such picture");
    let (variant, media) = match file.as_str() {
        "64.webp" => ("64", "image/webp"),
        "128.webp" => ("128", "image/webp"),
        "256.webp" => ("256", "image/webp"),
        "512.webp" => ("512", "image/webp"),
        "256.jpg" => ("jpeg", "image/jpeg"),
        _ => return not_found().into_response(),
    };
    if id.len() > 64 {
        return not_found().into_response();
    }
    let Ok(blobs) = storage(&state) else {
        return not_found().into_response();
    };
    let viewer = match viewer(&state, &headers).await {
        Ok(v) => v,
        Err(e) => return e.into_response(),
    };
    let row = match state.store.avatar_by_id(id).await {
        Ok(Some(row)) if row.version == version => row,
        Ok(_) => return not_found().into_response(),
        Err(_) => return ApiError::server("store_error").into_response(),
    };
    let owner = match state.store.get(row.owner.clone()).await {
        Ok(Some(owner)) => owner,
        _ => return not_found().into_response(),
    };
    if !feature_on(&state, owner.account_id.as_deref(), capabilities::AVATARS).await
        || !can_view(&state, &viewer, &owner, &row).await
    {
        return not_found().into_response();
    }
    let Some(bytes) = row.variants.get(variant).and_then(|d| blobs.get(d)) else {
        return not_found().into_response();
    };
    let cache = if row.visibility == Visibility::Everyone {
        "public, max-age=31536000, immutable"
    } else {
        "private, max-age=300"
    };
    let mut resp = bytes.into_response();
    let h = resp.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static(media));
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("default-src 'none'; sandbox"),
    );
    if row.visibility != Visibility::Everyone {
        h.insert(header::VARY, HeaderValue::from_static("Authorization"));
    }
    resp
}

#[derive(serde::Deserialize)]
pub(crate) struct ReportReq {
    address: String,
    #[serde(default)]
    identity: Option<String>,
}

/// `POST /v1/report-avatar` — report a picture to the operator.
pub(crate) async fn report(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<ReportReq>,
) -> Response {
    let me = match mailbox_for(
        &state,
        &headers,
        req.identity.as_deref(),
        Action::ReportAbuse,
    )
    .await
    {
        Ok((_, me)) => me,
        Err(e) => return e.into_response(),
    };
    let viewer = match viewer(&state, &headers).await {
        Ok(v) => v,
        Err(e) => return e.into_response(),
    };
    // Only a picture the reporter can actually see can be reported, and "none" is one answer.
    let row = match visible_avatar(&state, &viewer, &req.address).await {
        Ok(Some(row)) => row,
        Ok(None) => {
            return ApiError::new(StatusCode::NOT_FOUND, "not_found", "no picture to report")
                .into_response()
        }
        Err(e) => return e.into_response(),
    };
    if row.owner == me.address {
        return ApiError::bad("invalid_report", "you cannot report your own picture")
            .into_response();
    }
    match state
        .store
        .report_avatar(
            row.id.clone(),
            me.address.clone(),
            now_unix(),
            REPORTS_PER_DAY,
        )
        .await
    {
        Ok(Some(_)) => {
            tracing::info!(reporter = %me.address, avatar = %row.id, "avatar reported");
            Json(json!({ "reported": true })).into_response()
        }
        Ok(None) => ApiError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "rate_limited",
            "too many reports today",
        )
        .with_retry_after(3_600)
        .into_response(),
        Err(_) => ApiError::server("store_error").into_response(),
    }
}

// ---- operator (internal) -------------------------------------------------------------------------

/// `GET /internal/v1/avatar-reports` — reported pictures, most reported first.
pub(crate) async fn reports(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(resp) = crate::internal_api::authorize_service(
        &state,
        &headers,
        crate::internal_api::Endpoint::ModerateAvatars,
        Action::ModerateAvatars,
    ) {
        return *resp;
    }
    match state.store.reported_avatars().await {
        Ok(rows) => Json(json!({
            "reports": rows.iter().map(|(row, n)| json!({
                "avatar_id": row.id,
                "owner": row.owner,
                "visibility": row.visibility.as_str(),
                "reports": n,
                "urls": urls(row),
                "created_at": row.created_at,
            })).collect::<Vec<_>>()
        }))
        .into_response(),
        Err(_) => ApiError::server("store_error").into_response(),
    }
}

/// `POST /internal/v1/avatars/{id}/remove` — take a picture down after review.
pub(crate) async fn moderate_remove(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    if let Err(resp) = crate::internal_api::authorize_service(
        &state,
        &headers,
        crate::internal_api::Endpoint::ModerateAvatars,
        Action::ModerateAvatars,
    ) {
        return *resp;
    }
    let row = match state.store.avatar_by_id(id).await {
        Ok(Some(row)) => row,
        Ok(None) => {
            return ApiError::new(StatusCode::NOT_FOUND, "not_found", "no such picture")
                .into_response()
        }
        Err(_) => return ApiError::server("store_error").into_response(),
    };
    let Ok(Some(owner)) = state.store.get(row.owner.clone()).await else {
        return ApiError::server("store_error").into_response();
    };
    match state
        .store
        .remove_avatar(owner.address.clone(), "moderated", now_unix())
        .await
    {
        Ok((_, orphaned)) => {
            release(&state, &orphaned);
            announce(&state, &owner).await;
            tracing::warn!(avatar = %row.id, owner = %owner.address, "avatar removed by the operator");
            Json(json!({ "removed": true })).into_response()
        }
        Err(_) => ApiError::server("store_error").into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(w: u32, h: u32) -> Vec<u8> {
        let img = image::RgbaImage::from_fn(w, h, |x, y| {
            image::Rgba([
                (x * 7 % 256) as u8,
                (y * 13 % 256) as u8,
                ((x ^ y) % 256) as u8,
                255,
            ])
        });
        let mut out = Vec::new();
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
            .unwrap();
        out
    }

    fn jpeg_with_exif(w: u32, h: u32) -> Vec<u8> {
        let img = image::RgbImage::from_fn(w, h, |x, y| {
            image::Rgb([(x % 256) as u8, (y % 256) as u8, 90])
        });
        let mut plain = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut plain, 90)
            .encode_image(&img)
            .unwrap();
        // Splice an APP1 Exif segment carrying a recognisable "GPS" marker after SOI.
        let payload = b"Exif\0\0MM\0*\0\0\0\x08\0\0SECRET-GPS-46.2N-6.1E";
        let len = (payload.len() + 2) as u16;
        let mut out = vec![0xFF, 0xD8, 0xFF, 0xE1];
        out.extend_from_slice(&len.to_be_bytes());
        out.extend_from_slice(payload);
        out.extend_from_slice(&plain[2..]);
        out
    }

    #[test]
    fn crops_are_square_and_clamped() {
        assert_eq!(square(400, 300, None), (50, 0, 300));
        assert_eq!(square(300, 400, None), (0, 50, 300));
        assert_eq!(
            square(
                400,
                300,
                Some(Crop {
                    x: 390,
                    y: 290,
                    w: 100,
                    h: 80
                })
            ),
            (320, 220, 80)
        );
        assert_eq!(
            square(
                400,
                300,
                Some(Crop {
                    x: 0,
                    y: 0,
                    w: 5000,
                    h: 5000
                })
            ),
            (0, 0, 300)
        );
        assert!(Crop::parse("1,2,3").is_none());
        assert!(Crop::parse("1,2,0,4").is_none());
        assert!(Crop::parse("a,b,c,d").is_none());
        assert_eq!(
            Crop::parse("1, 2,3,4"),
            Some(Crop {
                x: 1,
                y: 2,
                w: 3,
                h: 4
            })
        );
    }

    #[test]
    fn a_picture_becomes_four_webps_and_a_jpeg_under_the_cap_without_metadata() {
        let out = process(
            &jpeg_with_exif(900, 600),
            Some(Crop {
                x: 100,
                y: 0,
                w: 600,
                h: 600,
            }),
        )
        .unwrap();
        assert_eq!(
            out.webp.iter().map(|(s, _)| *s).collect::<Vec<_>>(),
            vec![512, 256, 128, 64]
        );
        for (size, bytes) in &out.webp {
            assert!(bytes.len() <= VARIANT_MAX);
            assert_eq!(&bytes[0..4], b"RIFF");
            assert_eq!(&bytes[8..12], b"WEBP");
            let decoded = image::load_from_memory(bytes).unwrap();
            assert_eq!((decoded.width(), decoded.height()), (*size, *size));
            assert!(!bytes.windows(10).any(|w| w == b"SECRET-GPS"));
            // No EXIF/XMP chunk in the container.
            assert!(!bytes.windows(4).any(|w| w == b"EXIF" || w == b"XMP "));
        }
        assert!(out.jpeg.len() <= VARIANT_MAX);
        assert_eq!(&out.jpeg[0..2], &[0xFF, 0xD8]);
        assert!(!out.jpeg.windows(10).any(|w| w == b"SECRET-GPS"));
        assert!(!out.jpeg.windows(4).any(|w| w == b"Exif"));
        let j = image::load_from_memory(&out.jpeg).unwrap();
        assert_eq!((j.width(), j.height()), (256, 256));
    }

    #[test]
    fn noise_is_squeezed_under_the_cap() {
        // Random pixels are the worst case for size.
        let mut seed = 0x1234_5678u32;
        let img = image::RgbImage::from_fn(1024, 1024, |_, _| {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            image::Rgb([seed as u8, (seed >> 8) as u8, (seed >> 16) as u8])
        });
        let mut bytes = Vec::new();
        image::DynamicImage::ImageRgb8(img)
            .write_to(
                &mut std::io::Cursor::new(&mut bytes),
                image::ImageFormat::Png,
            )
            .unwrap();
        match process(&bytes, None) {
            Ok(out) => assert!(out.webp.iter().all(|(_, b)| b.len() <= VARIANT_MAX)),
            Err(AvatarError::Bad(_)) => {} // refused cleanly is also acceptable
            Err(e) => panic!("unexpected {e:?}"),
        }
    }

    #[test]
    fn hostile_and_unsupported_inputs_are_refused_cleanly() {
        // Not an image, a truncated one, a GIF, an oversized body.
        assert_eq!(
            process(b"hello world", None).unwrap_err(),
            AvatarError::Unsupported
        );
        let mut truncated = png(64, 64);
        truncated.truncate(truncated.len() / 2);
        assert!(matches!(
            process(&truncated, None),
            Err(AvatarError::Bad(_))
        ));
        assert_eq!(
            process(b"GIF89a\x01\x00\x01\x00\x00\x00\x00;", None).unwrap_err(),
            AvatarError::Unsupported
        );
        assert_eq!(
            process(&vec![0u8; MAX_UPLOAD + 1], None).unwrap_err(),
            AvatarError::TooLarge
        );
        // HEIC (an ISO-BMFF `ftypheic` box) is not decoded on the server.
        let heic = b"\0\0\0\x18ftypheic\0\0\0\0mif1heic";
        assert_eq!(process(heic, None).unwrap_err(), AvatarError::Unsupported);
    }

    /// A PNG whose header claims enormous dimensions but carries almost no data: the classic
    /// decompression bomb. It must be refused from the header, before anything is allocated.
    #[test]
    fn decompression_bombs_are_refused_from_the_header() {
        fn chunk(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
            let mut out = (data.len() as u32).to_be_bytes().to_vec();
            out.extend_from_slice(kind);
            out.extend_from_slice(data);
            let mut crc = crc32(kind);
            crc = crc32_update(crc, data);
            out.extend_from_slice(&(!crc).to_be_bytes());
            out
        }
        fn crc32(data: &[u8]) -> u32 {
            crc32_update(0xFFFF_FFFF, data)
        }
        fn crc32_update(mut crc: u32, data: &[u8]) -> u32 {
            for &b in data {
                crc ^= u32::from(b);
                for _ in 0..8 {
                    crc = if crc & 1 != 0 {
                        (crc >> 1) ^ 0xEDB8_8320
                    } else {
                        crc >> 1
                    };
                }
            }
            crc
        }
        for (w, h) in [(60_000u32, 60_000u32), (8_000, 8_000), (100_000, 1)] {
            let mut ihdr = Vec::new();
            ihdr.extend_from_slice(&w.to_be_bytes());
            ihdr.extend_from_slice(&h.to_be_bytes());
            ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
            let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
            png.extend(chunk(b"IHDR", &ihdr));
            png.extend(chunk(
                b"IDAT",
                &[0x78, 0x9c, 0x03, 0x00, 0x00, 0x00, 0x00, 0x01],
            ));
            png.extend(chunk(b"IEND", &[]));
            let started = std::time::Instant::now();
            assert!(
                matches!(process(&png, None), Err(AvatarError::Bad(_))),
                "{w}x{h}"
            );
            assert!(started.elapsed() < std::time::Duration::from_secs(2));
        }
    }

    #[test]
    fn pseudo_random_bytes_never_panic_the_decoder() {
        // A small deterministic fuzz: mutate valid images byte by byte and make sure every outcome
        // is a value, never a panic or a hang.
        let seeds = [png(40, 30), jpeg_with_exif(48, 32)];
        let mut state = 0x9E37_79B9u32;
        for seed in &seeds {
            for _ in 0..150 {
                let mut bytes = seed.clone();
                for _ in 0..8 {
                    state ^= state << 13;
                    state ^= state >> 17;
                    state ^= state << 5;
                    let at = 8 + (state as usize % (bytes.len() - 8));
                    bytes[at] = (state >> 24) as u8;
                }
                let _ = std::panic::catch_unwind(|| process(&bytes, None))
                    .expect("the decoder must not panic");
            }
        }
    }

    #[test]
    fn the_worker_wire_format_round_trips_and_rejects_garbage() {
        let out = process(&png(300, 300), None).unwrap();
        let wire = encode_output(&Ok(out));
        let back = decode_output(&wire).unwrap();
        assert_eq!(back.webp.len(), 4);
        assert_eq!(
            decode_output(&encode_output(&Err(AvatarError::Unsupported))).unwrap_err(),
            AvatarError::Unsupported
        );
        assert!(decode_output(&[]).is_err());
        assert!(decode_output(&[0, 0, 2, 0xFF, 0xFF, 0xFF, 0x7F]).is_err());
        assert!(decode_output(&wire[..wire.len() - 3]).is_err());
    }

    #[test]
    fn exif_orientation_is_applied_before_cropping() {
        // A 200x100 landscape with orientation 6 (rotate 90° CW) displays as 100x200 portrait; the
        // centre square is then taken from the portrait.
        let img = image::RgbImage::from_fn(200, 100, |x, _| {
            if x < 100 {
                image::Rgb([255, 0, 0])
            } else {
                image::Rgb([0, 0, 255])
            }
        });
        let mut plain = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut plain, 95)
            .encode_image(&img)
            .unwrap();
        // Minimal big-endian TIFF with one IFD entry: Orientation (0x0112) SHORT 1 = 6.
        let mut exif = b"Exif\0\0MM\0*\0\0\0\x08".to_vec();
        exif.extend_from_slice(&[
            0, 1, 0x01, 0x12, 0, 3, 0, 0, 0, 1, 0, 6, 0, 0, 0, 0, 0, 0, 0, 0,
        ]);
        let len = (exif.len() + 2) as u16;
        let mut bytes = vec![0xFF, 0xD8, 0xFF, 0xE1];
        bytes.extend_from_slice(&len.to_be_bytes());
        bytes.extend_from_slice(&exif);
        bytes.extend_from_slice(&plain[2..]);
        let out = process(&bytes, None).unwrap();
        let top = image::load_from_memory(&out.webp[0].1).unwrap().to_rgb8();
        // After a CW rotation the red (left) half is on top and blue at the bottom.
        let (t, b) = (top.get_pixel(256, 20), top.get_pixel(256, 490));
        assert!(t[0] > 150 && t[2] < 100, "top {t:?}");
        assert!(b[2] > 150 && b[0] < 100, "bottom {b:?}");
    }
}
