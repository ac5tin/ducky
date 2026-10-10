import type { ImagePart } from "./types";

/// Largest image file accepted from the webview, raw bytes on disk (what the
/// user sees in their file manager). The backend measures the base64
/// encoding of this same file, so the two caps agree.
export const MAX_IMAGE_FILE_BYTES = 5 * 1024 * 1024;

const SUPPORTED_IMAGE_TYPES = ["image/png", "image/jpeg", "image/gif", "image/webp"];

/**
 * Why this file cannot be attached, or null when it can. The same rules the
 * backend enforces, checked before any IPC so the toast is instant.
 */
export function imageRejection(file: {
  type: string;
  size: number;
}): string | null {
  if (!SUPPORTED_IMAGE_TYPES.includes(file.type)) {
    return `Unsupported image type: ${file.type || "unknown"}. Use PNG, JPEG, GIF or WebP.`;
  }
  if (file.size > MAX_IMAGE_FILE_BYTES) {
    return "Image is too large — the limit is 5 MB per image.";
  }
  return null;
}

/** The `data:` URL the preview thumbnail renders from. */
export function imagePreviewUrl(image: ImagePart): string {
  return `data:${image.media_type};base64,${image.data}`;
}

/**
 * Image files from a paste event's clipboard data. `items` is the primary
 * source; `files` is the fallback for webviews that populate only that.
 * WebKitGTK returns files with an empty `type` on some clips — the item's
 * MIME is the only correct one available, so the file is re-wrapped with it.
 */
export function clipboardImageFiles(clipboardData: {
  items?: ArrayLike<{
    kind: string;
    type: string;
    getAsFile(): File | null;
  }>;
  files?: ArrayLike<File>;
}): File[] {
  const out: File[] = [];
  const items = clipboardData.items ? Array.from(clipboardData.items) : [];
  for (const item of items) {
    if (item.kind !== "file" || !item.type.startsWith("image/")) continue;
    const file = item.getAsFile();
    if (!file) continue;
    out.push(file.type ? file : new File([file], file.name, { type: item.type }));
  }
  if (out.length > 0) return out;
  return clipboardData.files
    ? Array.from(clipboardData.files).filter((file) => file.type.startsWith("image/"))
    : [];
}

/**
 * Reads the first supported image off the async clipboard. WebKitGTK hands
 * the paste event an empty payload for image clips, so the bytes are only
 * reachable this way; the caller invokes it synchronously inside the paste
 * dispatch to keep the transient activation the read requires. Rejections
 * propagate — the caller decides what to tell the user.
 */
export async function readClipboardImage(reader: {
  read(): Promise<
    ArrayLike<{
      readonly types: readonly string[];
      getType(type: string): Promise<Blob>;
    }>
  >;
}): Promise<File | null> {
  const items = await reader.read();
  for (const item of Array.from(items)) {
    const type = item.types.find((t) => SUPPORTED_IMAGE_TYPES.includes(t));
    if (!type) continue;
    const extension = type.slice("image/".length);
    return new File([await item.getType(type)], `pasted-image.${extension}`, { type });
  }
  return null;
}

/** Reads an image file into an `ImagePart`, base64 and all. */
export function readImagePart(file: File): Promise<ImagePart> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => {
      const dataUrl = String(reader.result);
      resolve({ media_type: file.type, data: dataUrl.slice(dataUrl.indexOf(",") + 1) });
    };
    reader.onerror = () => reject(reader.error ?? new Error("Could not read the image."));
    reader.readAsDataURL(file);
  });
}
