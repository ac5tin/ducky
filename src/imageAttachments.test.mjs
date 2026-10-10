import test from "node:test";
import assert from "node:assert/strict";

import {
  MAX_IMAGE_FILE_BYTES,
  clipboardImageFiles,
  imagePreviewUrl,
  imageRejection,
  readClipboardImage,
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

test("imageRejection refuses a file over the on-disk cap", () => {
  assert.match(
    imageRejection({ type: "image/png", size: MAX_IMAGE_FILE_BYTES + 1 }),
    /too large/,
  );
  assert.equal(imageRejection({ type: "image/png", size: MAX_IMAGE_FILE_BYTES }), null);
});

test("imagePreviewUrl rebuilds the data URL the preview img needs", () => {
  assert.equal(
    imagePreviewUrl({ media_type: "image/webp", data: "eHk=" }),
    "data:image/webp;base64,eHk=",
  );
});

test("clipboardImageFiles reads image items and skips the rest", () => {
  const png = new File([new Uint8Array([1])], "shot.png", { type: "image/png" });
  const files = clipboardImageFiles({
    items: [
      { kind: "string", type: "text/plain", getAsFile: () => null },
      { kind: "file", type: "image/png", getAsFile: () => png },
    ],
  });
  assert.deepEqual(files.map((f) => f.name), ["shot.png"]);
});

test("clipboardImageFiles repairs WebKit's empty file type from the item", () => {
  // WebKitGTK hands back a File with no type on some clips; the item's MIME
  // is the only correct one available
  const typeless = new File([new Uint8Array([1])], "shot.png");
  assert.equal(typeless.type, "");
  const files = clipboardImageFiles({
    items: [{ kind: "file", type: "image/jpeg", getAsFile: () => typeless }],
  });
  assert.equal(files[0].type, "image/jpeg");
});

test("clipboardImageFiles falls back to files when items carry nothing", () => {
  const png = new File([new Uint8Array([1])], "shot.png", { type: "image/png" });
  const files = clipboardImageFiles({ files: [png] });
  assert.equal(files.length, 1);
  assert.equal(files[0].name, "shot.png");
  assert.deepEqual(clipboardImageFiles({}), []);
});

test("readClipboardImage picks the allowlisted type when an item offers several", async () => {
  const asked = [];
  const png = new Blob([new Uint8Array([1])], { type: "image/png" });
  const file = await readClipboardImage({
    read: async () => [
      {
        types: ["image/avif", "image/png", "text/html"],
        getType: async (type) => {
          asked.push(type);
          return png;
        },
      },
    ],
  });
  assert.deepEqual(asked, ["image/png"]);
  assert.equal(file?.type, "image/png");
  assert.equal(file?.name, "pasted-image.png");
});

test("readClipboardImage returns null when no item carries an allowlisted image", async () => {
  let asked = 0;
  const file = await readClipboardImage({
    read: async () => [
      {
        types: ["text/plain", "text/html"],
        getType: async () => {
          asked++;
          return new Blob(["hi"]);
        },
      },
      {
        types: ["image/avif"],
        getType: async () => {
          asked++;
          return new Blob([new Uint8Array([1])]);
        },
      },
    ],
  });
  assert.equal(file, null);
  assert.equal(asked, 0);
});

test("readClipboardImage wraps the blob as a File typed by the chosen MIME", async () => {
  const webp = new Blob([new Uint8Array([1, 2])], { type: "image/webp" });
  const file = await readClipboardImage({
    read: async () => [{ types: ["image/webp"], getType: async () => webp }],
  });
  assert.equal(file?.type, "image/webp");
  assert.equal(file?.name, "pasted-image.webp");
  assert.equal(file?.size, 2);
});

test("readClipboardImage propagates a reader rejection", async () => {
  await assert.rejects(
    readClipboardImage({
      read: async () => {
        throw new Error("NotAllowedError");
      },
    }),
    /NotAllowedError/,
  );
});
