// Square crop maths for profile pictures, kept apart from the page so it can be tested on its own.
//
// The picture is drawn into a square view of `view` pixels. At zoom 1 it covers the view exactly
// (its short side fills it); zoom scales up from there. `x`/`y` are where the picture's top-left
// corner sits in view pixels, always <= 0 so the view is never left partly empty. What the server
// needs back is the visible square in the picture's own pixels.
(function (root) {
  "use strict";

  function baseScale(imgW, imgH, view) {
    return view / Math.min(imgW, imgH);
  }

  // Keep the picture covering the view at this zoom.
  function clampOffset({ imgW, imgH, view, zoom, x, y }) {
    const s = baseScale(imgW, imgH, view) * zoom;
    const minX = view - imgW * s, minY = view - imgH * s;
    return { x: Math.min(0, Math.max(minX, x)), y: Math.min(0, Math.max(minY, y)) };
  }

  // Centre the picture at a zoom (used when the dialog opens and when zoom changes).
  function centred({ imgW, imgH, view, zoom }) {
    const s = baseScale(imgW, imgH, view) * zoom;
    return clampOffset({ imgW, imgH, view, zoom, x: (view - imgW * s) / 2, y: (view - imgH * s) / 2 });
  }

  // Zoom about the view's centre, keeping the point under it fixed.
  function rezoom({ imgW, imgH, view, from, to, x, y }) {
    const s0 = baseScale(imgW, imgH, view) * from, s1 = baseScale(imgW, imgH, view) * to;
    const cx = (view / 2 - x) / s0, cy = (view / 2 - y) / s0;
    return clampOffset({ imgW, imgH, view, zoom: to, x: view / 2 - cx * s1, y: view / 2 - cy * s1 });
  }

  // The visible square, in whole source pixels, inside the picture.
  function cropRect({ imgW, imgH, view, zoom, x, y }) {
    const at = clampOffset({ imgW, imgH, view, zoom, x, y });
    const s = baseScale(imgW, imgH, view) * zoom;
    let size = Math.max(1, Math.min(imgW, imgH, Math.round(view / s)));
    let left = Math.round(-at.x / s), top = Math.round(-at.y / s);
    left = Math.max(0, Math.min(imgW - size, left));
    top = Math.max(0, Math.min(imgH - size, top));
    return { x: left, y: top, w: size, h: size };
  }

  // A source rectangle re-expressed after the picture was downscaled by `factor` (< 1).
  function scaleRect(rect, factor, imgW, imgH) {
    const size = Math.max(1, Math.min(imgW, imgH, Math.round(rect.w * factor)));
    return {
      x: Math.max(0, Math.min(imgW - size, Math.round(rect.x * factor))),
      y: Math.max(0, Math.min(imgH - size, Math.round(rect.y * factor))),
      w: size, h: size,
    };
  }

  root.PigeonpostCrop = { baseScale, clampOffset, centred, rezoom, cropRect, scaleRect };
})(typeof window !== "undefined" ? window : globalThis);
