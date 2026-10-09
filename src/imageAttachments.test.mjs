import test from "node:test";
import assert from "node:assert/strict";

import {
  MAX_IMAGE_BASE64_BYTES,
  imageRejection,
  imagePreviewUrl,
} from "./imageAttachments.ts";

test("imageRejection accepts the four web image types", () => {
  for (const type of ["image/png", "image/jpeg", "image/gif", "image/webp"]) {
    assert.equal(imageRejection({ type, size: 1024 }), null, type);
  }
});

test("imageRejection names the refused type", () => {
  assert.match(imageRejection({ type: "image/svg+xml", size: 1024 }), /svg/);
  assert.match(imageRejection({ type: "", size: 1024 }), /PNG, JPEG, GIF or WebP/);
});

test("imageRejection refuses a file whose base64 payload would exceed the cap", () => {
  // the smallest raw size whose base64 encoding lands one byte over the cap
  const over = Math.floor((MAX_IMAGE_BASE64_BYTES / 4) * 3) + 1;
  assert.match(imageRejection({ type: "image/png", size: over }), /too large/);
  const under = Math.floor((MAX_IMAGE_BASE64_BYTES / 4) * 3);
  assert.equal(imageRejection({ type: "image/png", size: under }), null);
});

test("imagePreviewUrl rebuilds the data URL the preview img needs", () => {
  assert.equal(
    imagePreviewUrl({ media_type: "image/webp", data: "eHk=" }),
    "data:image/webp;base64,eHk=",
  );
});
