//! The real binary's `--avatar-worker` mode: one picture in on stdin, variants out on stdout.
//! This is the process the postbox runs under resource limits for every upload.

use std::io::Write;
use std::process::{Command, Stdio};

fn run(input: &[u8], args: &[&str]) -> Vec<u8> {
    let mut child = Command::new(env!("CARGO_BIN_EXE_pigeonpost-postbox"))
        .arg("--avatar-worker")
        .args(args)
        .env_clear()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("the worker starts");
    child.stdin.take().unwrap().write_all(input).unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success());
    out.stdout
}

fn png() -> Vec<u8> {
    let img = image::RgbaImage::from_fn(300, 200, |x, y| {
        image::Rgba([(x % 256) as u8, (y % 256) as u8, 60, 255])
    });
    let mut out = Vec::new();
    image::DynamicImage::ImageRgba8(img)
        .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
        .unwrap();
    out
}

#[test]
fn the_worker_turns_a_picture_into_variants() {
    let out = run(&png(), &["10,0,180,180"]);
    assert_eq!(out[0], 0, "success marker");
    // Five variants: four WebP and one JPEG.
    let mut at = 1;
    let mut sizes = Vec::new();
    while at < out.len() {
        let size = u16::from_le_bytes([out[at], out[at + 1]]);
        let len = u32::from_le_bytes([out[at + 2], out[at + 3], out[at + 4], out[at + 5]]) as usize;
        at += 6;
        let bytes = &out[at..at + len];
        at += len;
        if size == 0 {
            assert_eq!(&bytes[..2], &[0xFF, 0xD8]);
        } else {
            assert_eq!(&bytes[8..12], b"WEBP");
        }
        sizes.push(size);
    }
    assert_eq!(sizes, vec![512, 256, 128, 64, 0]);
}

#[test]
fn the_worker_reports_garbage_as_an_error_not_a_crash() {
    assert_eq!(run(b"definitely not a picture", &[]), vec![1, 2]);
}
