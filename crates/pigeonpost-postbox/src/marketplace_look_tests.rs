//! The marketplace look over real sockets: listing logos (validation, re-encoding, public
//! immutable URLs, replacement), storefront details, the `coming_soon` refusal on every buy and
//! run route, and the idempotent sample seed. Synthetic accounts and synthetic images only.

use crate::marketplace_listings::check_logo;
use crate::marketplace_m2_tests::*;
use crate::org_tests::{serve_with, Person, Srv, OPS};
use crate::*;
use reqwest::Method;
use serde_json::{json, Value};

fn png(w: u32, h: u32) -> Vec<u8> {
    let img = image::RgbaImage::from_fn(w, h, |x, y| {
        image::Rgba([(x * 7 % 256) as u8, (y * 13 % 256) as u8, 120, 255])
    });
    let mut out = Vec::new();
    image::DynamicImage::ImageRgba8(img)
        .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
        .unwrap();
    out
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for b in bytes {
        crc ^= u32::from(*b);
        for _ in 0..8 {
            crc = if crc & 1 == 1 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

/// A PNG with a `tEXt` chunk carrying a recognisable marker, right after the header chunk.
fn png_with_text(w: u32, h: u32, marker: &[u8]) -> Vec<u8> {
    let plain = png(w, h);
    // Signature (8) + IHDR (4 len + 4 type + 13 data + 4 crc).
    let at = 8 + 25;
    let mut data = b"Comment\0".to_vec();
    data.extend_from_slice(marker);
    let mut chunk = (data.len() as u32).to_be_bytes().to_vec();
    let mut typed = b"tEXt".to_vec();
    typed.extend_from_slice(&data);
    chunk.extend_from_slice(&typed);
    chunk.extend_from_slice(&crc32(&typed).to_be_bytes());
    let mut out = plain[..at].to_vec();
    out.extend_from_slice(&chunk);
    out.extend_from_slice(&plain[at..]);
    out
}

#[test]
fn logos_are_square_raster_images_of_at_most_a_megabyte_and_never_svg() {
    let ok = png(512, 512);
    assert!(check_logo("image/png", &ok).is_ok());
    assert!(check_logo("application/octet-stream", &ok).is_ok());
    let svg = br#"<?xml version="1.0"?><svg xmlns="http://www.w3.org/2000/svg"><script>alert(1)</script></svg>"#;
    assert_eq!(
        check_logo("image/svg+xml", svg).unwrap_err().code,
        "logo_unsupported"
    );
    // An SVG sent under a raster type is still refused.
    assert_eq!(
        check_logo("image/png", svg).unwrap_err().code,
        "logo_unsupported"
    );
    assert_eq!(
        check_logo("image/png", b"  \n<svg/>").unwrap_err().code,
        "logo_unsupported"
    );
    assert_eq!(
        check_logo("image/gif", b"GIF89a\x01\x00\x01\x00")
            .unwrap_err()
            .code,
        "logo_unsupported"
    );
    let mut gif = b"GIF89a".to_vec();
    gif.extend_from_slice(&[0; 32]);
    assert_eq!(
        check_logo("image/png", &gif).unwrap_err().code,
        "logo_unsupported"
    );
    let big = vec![0u8; crate::marketplace_listings::LOGO_MAX + 1];
    let e = check_logo("image/png", &big).unwrap_err();
    assert_eq!((e.code, e.status.as_u16()), ("logo_too_large", 413));
    assert_eq!(
        check_logo("image/png", &png(512, 300)).unwrap_err().code,
        "logo_not_square"
    );
    assert_eq!(
        check_logo("image/png", &png(128, 128)).unwrap_err().code,
        "logo_too_small"
    );
    assert_eq!(
        check_logo("image/png", b"").unwrap_err().code,
        "logo_unreadable"
    );
}

#[test]
fn a_logo_is_re_encoded_without_its_metadata() {
    let marker = b"SECRET-LOGO-METADATA-0042";
    let input = png_with_text(512, 512, marker);
    assert!(input.windows(marker.len()).any(|w| w == marker));
    assert!(check_logo("image/png", &input).is_ok());
    let out = crate::avatars::process(&input, None).unwrap();
    let sizes: Vec<u32> = out.webp.iter().map(|(s, _)| *s).collect();
    for size in crate::marketplace_listings::LOGO_SIZES {
        assert!(sizes.contains(&size), "{size}");
    }
    for (size, bytes) in &out.webp {
        assert_eq!(&bytes[..4], b"RIFF", "{size}");
        assert_eq!(&bytes[8..12], b"WEBP", "{size}");
        assert!(!bytes.windows(marker.len()).any(|w| w == marker));
        let decoded = image::load_from_memory(bytes).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (*size, *size));
    }
}

/// A marketplace postbox with a logo store.
async fn look_setup() -> (Srv, Person) {
    let srv = serve_with(|s| {
        s.platform_owner = Some(Arc::from(OWNER_ACCOUNT));
        let mut rt = nests::Runtime::capture();
        rt.mp.git_hosts.push("git.example.org".into());
        s.nests = Arc::new(rt);
        s.logo_blobs = Some(Arc::new(crate::blobs::Blobs::for_test("logos")));
    })
    .await;
    let owner = srv.person("mpowner").await;
    srv.global_flag("marketplace").await;
    (srv, owner)
}

async fn put_logo(
    srv: &Srv,
    token: &str,
    path: &str,
    content_type: &str,
    body: Vec<u8>,
) -> (u16, Value) {
    let r = srv
        .http
        .put(format!("{}{path}", srv.public))
        .header("x-forwarded-for", &srv.ip)
        .header("content-type", content_type)
        .bearer_auth(token)
        .body(body)
        .send()
        .await
        .unwrap();
    let s = r.status().as_u16();
    (s, r.json().await.unwrap_or(Value::Null))
}

async fn fetch(srv: &Srv, path: &str) -> (u16, reqwest::header::HeaderMap, Vec<u8>) {
    let r = srv
        .http
        .get(format!("{}{path}", srv.public))
        .header("x-forwarded-for", &srv.ip)
        .send()
        .await
        .unwrap();
    let s = r.status().as_u16();
    let h = r.headers().clone();
    (s, h, r.bytes().await.unwrap().to_vec())
}

#[tokio::test]
async fn publishers_upload_a_logo_that_is_served_publicly_and_replaced_by_url() {
    let (srv, owner) = look_setup().await;
    let rev = reviewer(&srv, &owner, "lkrev").await;
    let ann = srv.person("lkann").await;
    let p = certified(&srv, &rev, &ann).await;
    let t = template(&srv, &ann, &p, "team-digest").await;
    let b = attested(&srv, &rev, &ann, &p, &t, "1.0.0", None, 'a').await;
    let l = listing(&srv, &ann, &p, &b.release, "public").await;
    let path = format!("/v1/publishers/{p}/listings/{l}/logo");

    // Someone off the team cannot touch it; SVG and non-square images are refused.
    let eve = srv.person("lkeve").await;
    let (s, _) = put_logo(&srv, &eve.token, &path, "image/png", png(512, 512)).await;
    assert_eq!(s, 404);
    let (s, v) = put_logo(&srv, &ann.token, &path, "image/svg+xml", b"<svg/>".to_vec()).await;
    assert_eq!((s, code(&v)), (415, "logo_unsupported"), "{v}");
    let (s, v) = put_logo(&srv, &ann.token, &path, "image/png", png(600, 512)).await;
    assert_eq!((s, code(&v)), (422, "logo_not_square"), "{v}");

    // A draft listing already shows its logo to its publisher.
    let (s, v) = put_logo(&srv, &ann.token, &path, "image/png", png(512, 512)).await;
    assert_eq!(s, 200, "{v}");
    let first = v["data"]["logo"]["urls"]["128"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(first.starts_with(&format!("/v1/marketplace/listings/{l}/logo/")));
    let (_, mine) = srv
        .get(&format!("/v1/publishers/{p}/listings"), &ann.token)
        .await;
    assert_eq!(mine["data"][0]["logo"]["urls"]["128"], first.as_str());

    // Public, immutable, sandboxed WebP, fetched without any credential.
    let (s, h, bytes) = fetch(&srv, &first).await;
    assert_eq!(s, 200);
    assert_eq!(h["content-type"], "image/webp");
    assert_eq!(h["cache-control"], "public, max-age=31536000, immutable");
    assert_eq!(h["x-content-type-options"], "nosniff");
    assert_eq!(image::load_from_memory(&bytes).unwrap().width(), 128);
    for bad in [
        first.replace("128.webp", "256.webp"),
        first.replace("128.webp", "128.png"),
        format!("/v1/marketplace/listings/{l}/logo/0000000000000000/128.webp"),
        first.replace(&l, "lst_missing"),
    ] {
        assert_eq!(fetch(&srv, &bad).await.0, 404, "{bad}");
    }

    // Published, the catalog and the detail carry it, with the verified publisher badge.
    let (s, v) = publish(&srv, &ann.strong, &p, &l, json!({})).await;
    assert_eq!(s, 200, "{v}");
    let (_, cat) = srv.get("/v1/marketplace/listings", &eve.token).await;
    let row = &cat["data"][0];
    assert_eq!(row["logo"]["urls"]["128"], first.as_str());
    assert_eq!(row["publisher"]["verified"], true);
    assert_eq!(row["coming_soon"], false);
    let (_, detail) = srv
        .get(&format!("/v1/marketplace/listings/{l}"), &eve.token)
        .await;
    assert!(detail["data"]["logo"]["urls"]["512"].as_str().is_some());

    // A new logo is a new URL; the old one stops answering.
    let other = image::RgbaImage::from_pixel(400, 400, image::Rgba([10, 200, 90, 255]));
    let mut other_png = Vec::new();
    image::DynamicImage::ImageRgba8(other)
        .write_to(
            &mut std::io::Cursor::new(&mut other_png),
            image::ImageFormat::Png,
        )
        .unwrap();
    let (s, v) = put_logo(&srv, &ann.token, &path, "image/png", other_png).await;
    assert_eq!(s, 200, "{v}");
    let second = v["data"]["logo"]["urls"]["128"]
        .as_str()
        .unwrap()
        .to_string();
    assert_ne!(first, second);
    assert_eq!(fetch(&srv, &first).await.0, 404);
    assert_eq!(fetch(&srv, &second).await.0, 200);

    // Removed, the listing is drawn with the default again.
    let (s, v) = srv
        .call(Method::DELETE, &path, Some(&ann.token), None)
        .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(fetch(&srv, &second).await.0, 404);
    let (_, detail) = srv
        .get(&format!("/v1/marketplace/listings/{l}"), &eve.token)
        .await;
    assert_eq!(detail["data"]["logo"], Value::Null);

    // Storefront details: a known category and https links only; null clears.
    let sf = format!("/v1/publishers/{p}/listings/{l}/storefront");
    let (s, v) = srv
        .call(
            Method::PUT,
            &sf,
            Some(&ann.token),
            Some(json!({ "category": "productivity", "support_url": "https://acme.example/help" })),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["data"]["category"], "productivity");
    assert_eq!(v["data"]["support_url"], "https://acme.example/help");
    for bad in [
        json!({ "category": "games" }),
        json!({ "support_url": "http://acme.example" }),
        json!({ "privacy_url": "javascript:alert(1)" }),
        json!({ "colour": "red" }),
    ] {
        let (s, _) = srv
            .call(Method::PUT, &sf, Some(&ann.token), Some(bad.clone()))
            .await;
        assert_eq!(s, 400, "{bad}");
    }
    let (_, v) = srv
        .call(
            Method::PUT,
            &sf,
            Some(&ann.token),
            Some(json!({ "support_url": null })),
        )
        .await;
    assert_eq!(v["data"]["support_url"], Value::Null);
    assert_eq!(v["data"]["category"], "productivity");
}

#[tokio::test]
async fn samples_seed_once_show_as_coming_soon_and_refuse_every_buy_and_run() {
    let (srv, owner) = look_setup().await;
    let seed = |m: Method| srv.call(m, "/internal/v1/marketplace/samples", Some(OPS), None);
    let (s, _) = srv
        .call(Method::POST, "/internal/v1/marketplace/samples", None, None)
        .await;
    assert_eq!(s, 401);
    let (s, v) = seed(Method::POST).await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(
        (
            v["data"]["created"].as_u64(),
            v["data"]["existing"].as_u64()
        ),
        (Some(9), Some(0))
    );
    // Idempotent: a second run creates nothing.
    let (_, again) = seed(Method::POST).await;
    assert_eq!(
        (
            again["data"]["created"].as_u64(),
            again["data"]["existing"].as_u64()
        ),
        (Some(0), Some(9))
    );
    assert_eq!(again["data"]["publisher_id"], v["data"]["publisher_id"]);

    let bob = srv.person("lkbob").await;
    let (_, cat) = srv.get("/v1/marketplace/listings", &bob.token).await;
    let rows = cat["data"].as_array().unwrap();
    assert_eq!(rows.len(), 9, "{cat}");
    assert!(rows
        .iter()
        .all(|r| r["coming_soon"] == true && r["sample"] == true));
    assert!(rows.iter().all(|r| r["publisher"]["first_party"] == true));
    assert!(rows
        .iter()
        .all(|r| r["category"].is_string() && r["logo"].is_null()));
    assert_eq!(rows.iter().filter(|r| r["featured"] == true).count(), 3);
    assert_eq!(rows[0]["title"], "Contract reviewer");
    let id = rows[0]["id"].as_str().unwrap().to_string();
    let (s, detail) = srv
        .get(&format!("/v1/marketplace/listings/{id}"), &bob.token)
        .await;
    assert_eq!(s, 200, "{detail}");
    assert!(detail["data"]["description"]
        .as_str()
        .unwrap()
        .contains("sample listing from Pigeonpost"));

    // Nothing can be bought, installed or run from a coming-soon listing.
    for (path, body) in [
        ("/v1/nest-installations", json!({ "listing_id": id })),
        (
            "/v1/marketplace/purchases",
            json!({ "listing_id": id, "owner_kind": "account", "owner_id": bob.account,
                    "intent_id": "ab".repeat(16) }),
        ),
        (
            "/v1/marketplace/hosted-jobs",
            json!({ "listing_id": id, "operation": "contract.review", "input": {} }),
        ),
    ] {
        let (s, v) = srv.post(path, &bob.token, body).await;
        assert_eq!((s, code(&v)), (409, "coming_soon"), "{path}: {v}");
    }

    // Withdrawn, they leave the catalog, and a later seed does not bring them back.
    let (s, v) = seed(Method::DELETE).await;
    assert_eq!((s, v["data"]["withdrawn"].as_u64()), (200, Some(9)), "{v}");
    let (_, cat) = srv.get("/v1/marketplace/listings", &bob.token).await;
    assert_eq!(cat["data"].as_array().unwrap().len(), 0);
    let (_, again) = seed(Method::POST).await;
    assert_eq!(again["data"]["created"].as_u64(), Some(0));
    let (_, cat) = srv.get("/v1/marketplace/listings", &bob.token).await;
    assert_eq!(cat["data"].as_array().unwrap().len(), 0);
    let _ = owner;
}
